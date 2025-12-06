//! Router task that fans out audio to sinks.

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::sink::Sink;
use crate::{AudioChunk, EventCallback, StreamConfig, StreamEvent};

/// Command sent to the router task.
pub enum RouterCommand {
    /// Stop the router gracefully.
    Stop,
}

/// The router receives audio chunks and forwards them to all sinks.
pub struct Router {
    sinks: Vec<Arc<dyn Sink>>,
    event_callback: Option<EventCallback>,
    config: StreamConfig,
}

impl Router {
    /// Creates a new router with the given sinks.
    pub fn new(sinks: Vec<Arc<dyn Sink>>, config: StreamConfig) -> Self {
        Self {
            sinks,
            event_callback: None,
            config,
        }
    }

    /// Sets the event callback.
    #[allow(dead_code)] // Will be used by builder
    pub fn with_event_callback(mut self, callback: EventCallback) -> Self {
        self.event_callback = Some(callback);
        self
    }

    /// Sends an event to the callback if configured.
    fn emit_event(&self, event: StreamEvent) {
        if let Some(ref callback) = self.event_callback {
            callback(event);
        }
    }

    /// Writes a chunk to a single sink with retry logic.
    async fn write_to_sink(&self, sink: &Arc<dyn Sink>, chunk: &AudioChunk) {
        let mut attempts = 0;
        let mut delay = self.config.sink_retry_delay;

        loop {
            match sink.write(chunk).await {
                Ok(()) => return,
                Err(e) => {
                    attempts += 1;
                    self.emit_event(StreamEvent::SinkError {
                        sink_name: sink.name().to_string(),
                        error: e.to_string(),
                    });

                    if attempts >= self.config.sink_retry_attempts {
                        // Max retries reached, give up on this chunk
                        return;
                    }

                    // Exponential backoff
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
    }

    /// Writes a chunk to all sinks concurrently.
    pub async fn write_chunk(&self, chunk: &AudioChunk) {
        let futures: Vec<_> = self
            .sinks
            .iter()
            .map(|sink| self.write_to_sink(sink, chunk))
            .collect();

        futures::future::join_all(futures).await;
    }

    /// Starts all sinks.
    ///
    /// Returns an error if any sink fails to start.
    #[allow(dead_code)] // Will be used by builder
    pub async fn start_sinks(&self) -> Result<(), crate::StreamAudioError> {
        for sink in &self.sinks {
            sink.on_start()
                .await
                .map_err(|e| crate::StreamAudioError::SinkStartFailed {
                    sink_name: sink.name().to_string(),
                    reason: e.to_string(),
                })?;
        }
        Ok(())
    }

    /// Stops all sinks.
    pub async fn stop_sinks(&self) {
        for sink in &self.sinks {
            if let Err(e) = sink.on_stop().await {
                self.emit_event(StreamEvent::SinkError {
                    sink_name: sink.name().to_string(),
                    error: format!("Error during shutdown: {e}"),
                });
            }
        }
    }

    /// Runs the router, reading from a channel and writing to sinks.
    ///
    /// This is the main entry point for the router task.
    pub async fn run(
        self,
        mut chunk_rx: mpsc::Receiver<AudioChunk>,
        mut cmd_rx: mpsc::Receiver<RouterCommand>,
    ) {
        loop {
            tokio::select! {
                Some(chunk) = chunk_rx.recv() => {
                    self.write_chunk(&chunk).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        RouterCommand::Stop => {
                            // Drain remaining chunks
                            while let Ok(chunk) = chunk_rx.try_recv() {
                                self.write_chunk(&chunk).await;
                            }
                            break;
                        }
                    }
                }
                else => break,
            }
        }

        self.stop_sinks().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SinkError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct TestSink {
        name: String,
        write_count: AtomicUsize,
        fail_count: AtomicUsize,
    }

    impl TestSink {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                write_count: AtomicUsize::new(0),
                fail_count: AtomicUsize::new(0),
            }
        }

        fn failing(name: &str, fail_times: usize) -> Self {
            Self {
                name: name.to_string(),
                write_count: AtomicUsize::new(0),
                fail_count: AtomicUsize::new(fail_times),
            }
        }

        fn writes(&self) -> usize {
            self.write_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Sink for TestSink {
        fn name(&self) -> &str {
            &self.name
        }

        async fn write(&self, _chunk: &AudioChunk) -> Result<(), SinkError> {
            let remaining = self.fail_count.load(Ordering::SeqCst);
            if remaining > 0 {
                self.fail_count.fetch_sub(1, Ordering::SeqCst);
                return Err(SinkError::custom("intentional failure"));
            }
            self.write_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_router_writes_to_all_sinks() {
        let sink1 = Arc::new(TestSink::new("sink1"));
        let sink2 = Arc::new(TestSink::new("sink2"));

        let router = Router::new(vec![sink1.clone(), sink2.clone()], StreamConfig::default());

        let chunk = AudioChunk::new(vec![0; 100], Duration::ZERO, 16000, 1);
        router.write_chunk(&chunk).await;

        assert_eq!(sink1.writes(), 1);
        assert_eq!(sink2.writes(), 1);
    }

    #[tokio::test]
    async fn test_router_retries_on_failure() {
        let sink = Arc::new(TestSink::failing("sink", 2)); // Fail twice, then succeed

        let mut config = StreamConfig::default();
        config.sink_retry_delay = Duration::from_millis(1); // Fast for testing

        let router = Router::new(vec![sink.clone()], config);

        let chunk = AudioChunk::new(vec![0; 100], Duration::ZERO, 16000, 1);
        router.write_chunk(&chunk).await;

        assert_eq!(sink.writes(), 1); // Should succeed on 3rd attempt
    }

    #[tokio::test]
    async fn test_router_run_stops_on_command() {
        let sink = Arc::new(TestSink::new("sink"));
        let router = Router::new(vec![sink.clone()], StreamConfig::default());

        let (chunk_tx, chunk_rx) = mpsc::channel(10);
        let (cmd_tx, cmd_rx) = mpsc::channel(1);

        // Send a chunk
        let chunk = AudioChunk::new(vec![0; 100], Duration::ZERO, 16000, 1);
        chunk_tx.send(chunk).await.unwrap();

        // Send stop command
        cmd_tx.send(RouterCommand::Stop).await.unwrap();

        // Run router
        router.run(chunk_rx, cmd_rx).await;

        assert_eq!(sink.writes(), 1);
    }
}
