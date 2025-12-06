//! Router task that fans out audio to sinks.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::merger::TimeWindowMerger;
use super::routing::{RoutingTable, SinkRoute};
use crate::sink::Sink;
use crate::source::SourceId;
use crate::{AudioChunk, EventCallback, StreamConfig, StreamEvent};

/// Command sent to the router task.
pub enum RouterCommand {
    /// Stop the router gracefully.
    Stop,
}

/// The router receives audio chunks and forwards them to all sinks.
///
/// In single-source mode (v0.1 compatible), all chunks go to all sinks.
/// In multi-source mode, chunks are routed based on the routing table.
pub struct Router {
    sinks: Vec<Arc<dyn Sink>>,
    /// Routing table for efficient dispatch (only in multi-source mode).
    routing_table: Option<RoutingTable>,
    /// Merger for combined sinks (only when merge routes exist).
    merger: Option<Mutex<TimeWindowMerger>>,
    event_callback: Option<EventCallback>,
    config: StreamConfig,
}

impl Router {
    /// Creates a new router with the given sinks (single-source mode).
    pub fn new(sinks: Vec<Arc<dyn Sink>>, config: StreamConfig) -> Self {
        Self {
            sinks,
            routing_table: None,
            merger: None,
            event_callback: None,
            config,
        }
    }

    /// Creates a new router with routing (multi-source mode).
    ///
    /// # Arguments
    ///
    /// * `sample_rate` - Target sample rate for merged audio output
    /// * `channels` - Target channel count for merged audio output
    pub fn with_routing(
        sinks: Vec<Arc<dyn Sink>>,
        sink_routes: &[SinkRoute],
        source_ids: Vec<SourceId>,
        config: StreamConfig,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, crate::StreamAudioError> {
        // Build routing table from sink_routes
        let routing_table =
            RoutingTable::new(sink_routes.iter().enumerate(), source_ids.iter().cloned())?;

        // Create merger if there are merge routes
        let merger = if routing_table.has_merge_routes() {
            Some(Mutex::new(TimeWindowMerger::new(
                config.merge_window_duration,
                config.merge_window_timeout,
                source_ids,
                sample_rate,
                channels,
            )))
        } else {
            None
        };

        Ok(Self {
            sinks,
            routing_table: Some(routing_table),
            merger,
            event_callback: None,
            config,
        })
    }

    /// Sets the event callback.
    pub fn with_event_callback(mut self, callback: EventCallback) -> Self {
        self.event_callback = Some(callback);
        self
    }

    /// Returns true if this is a multi-source router (for introspection).
    #[allow(dead_code)]
    pub fn is_multi_source(&self) -> bool {
        self.routing_table.is_some()
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

    /// Writes a chunk to specific sink indices.
    async fn write_to_indices(&self, chunk: &AudioChunk, indices: &[usize]) {
        let futures: Vec<_> = indices
            .iter()
            .filter_map(|&i| self.sinks.get(i))
            .map(|sink| self.write_to_sink(sink, chunk))
            .collect();

        futures::future::join_all(futures).await;
    }

    /// Writes a chunk to all sinks (single-source mode).
    pub async fn write_chunk(&self, chunk: &AudioChunk) {
        if let Some(ref table) = self.routing_table {
            // Multi-source routing
            self.route_chunk(chunk, table).await;
        } else {
            // Single-source: all sinks get all chunks
            let futures: Vec<_> = self
                .sinks
                .iter()
                .map(|sink| self.write_to_sink(sink, chunk))
                .collect();

            futures::future::join_all(futures).await;
        }
    }

    /// Routes a chunk based on the routing table.
    async fn route_chunk(&self, chunk: &AudioChunk, table: &RoutingTable) {
        // Handle broadcast sinks (receive all)
        let broadcast = table.broadcast_sinks();
        if !broadcast.is_empty() {
            self.write_to_indices(chunk, broadcast).await;
        }

        // Handle direct routing (single source to sink)
        if let Some(source_id) = &chunk.source_id {
            let direct = table.direct_sinks(source_id);
            if !direct.is_empty() {
                self.write_to_indices(chunk, direct).await;
            }

            // Feed to merger if applicable
            if let Some(ref merger_mutex) = self.merger {
                let mut merger = merger_mutex.lock().await;
                if let Some(result) = merger.add_chunk(Arc::new(chunk.clone())) {
                    // Merged chunk ready - send to merge sinks
                    drop(merger); // Release lock before async write
                    self.write_merged_chunk(&result.chunk, table).await;

                    if !result.is_complete() {
                        self.emit_event(StreamEvent::MergeIncomplete {
                            window_id: result.window_id,
                            missing: result.missing,
                        });
                    }
                }
            }
        }
    }

    /// Writes a merged chunk to all sinks that want merged output.
    async fn write_merged_chunk(&self, chunk: &AudioChunk, table: &RoutingTable) {
        for group in table.merge_groups() {
            self.write_to_indices(chunk, &group.sink_indices).await;
        }
    }

    /// Checks for timed-out merge windows and emits them.
    pub async fn check_merge_timeouts(&self) {
        if let Some(ref merger_mutex) = self.merger {
            let mut merger = merger_mutex.lock().await;
            let results = merger.check_timeouts();
            drop(merger); // Release lock

            if let Some(ref table) = self.routing_table {
                for result in results {
                    self.write_merged_chunk(&result.chunk, table).await;

                    if !result.is_complete() {
                        self.emit_event(StreamEvent::MergeIncomplete {
                            window_id: result.window_id,
                            missing: result.missing,
                        });
                    }
                }
            }
        }
    }

    /// Starts all sinks.
    ///
    /// Returns an error if any sink fails to start.
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
        // Create timeout check interval only if we have merge routes
        let timeout_interval = if self.merger.is_some() {
            Some(tokio::time::interval(self.config.merge_window_timeout / 2))
        } else {
            None
        };

        // Use a pinned interval for tokio::select!
        tokio::pin!(timeout_interval);

        loop {
            tokio::select! {
                Some(chunk) = chunk_rx.recv() => {
                    self.write_chunk(&chunk).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        RouterCommand::Stop => {
                            // Check final timeouts
                            self.check_merge_timeouts().await;
                            // Drain remaining chunks
                            while let Ok(chunk) = chunk_rx.try_recv() {
                                self.write_chunk(&chunk).await;
                            }
                            break;
                        }
                    }
                }
                // Check merge timeouts periodically (only if merger exists)
                _ = async {
                    if let Some(ref mut interval) = timeout_interval.as_mut().as_pin_mut() {
                        interval.tick().await
                    } else {
                        std::future::pending::<tokio::time::Instant>().await
                    }
                } => {
                    self.check_merge_timeouts().await;
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
