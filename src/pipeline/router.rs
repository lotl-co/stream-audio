//! Router task that fans out audio to sinks.

use std::sync::Arc;
use tokio::sync::mpsc;

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
    event_callback: Option<EventCallback>,
    config: StreamConfig,
}

impl Router {
    /// Creates a new router with the given sinks (simple mode, test helper).
    #[cfg(test)]
    pub fn new(sinks: Vec<Arc<dyn Sink>>, config: StreamConfig) -> Self {
        Self {
            sinks,
            routing_table: None,
            event_callback: None,
            config,
        }
    }

    /// Creates a new router with routing (multi-source mode).
    ///
    /// # Arguments
    ///
    /// * `sinks` - Sinks to route audio to
    /// * `sink_routes` - Routing configuration for each sink
    /// * `source_ids` - IDs of all audio sources
    /// * `config` - Stream configuration
    pub fn with_routing(
        sinks: Vec<Arc<dyn Sink>>,
        sink_routes: &[SinkRoute],
        source_ids: &[SourceId],
        config: StreamConfig,
    ) -> Result<Self, crate::StreamAudioError> {
        // Build routing table from sink_routes
        let routing_table =
            RoutingTable::new(sink_routes.iter().enumerate(), source_ids.iter().cloned())?;

        Ok(Self {
            sinks,
            routing_table: Some(routing_table),
            event_callback: None,
            config,
        })
    }

    /// Sets the event callback.
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
            tracing::trace!(
                "Router: source={}, direct_sinks={:?}, ts={:?}, samples={}",
                source_id,
                direct,
                chunk.timestamp,
                chunk.samples.len()
            );
            if !direct.is_empty() {
                self.write_to_indices(chunk, direct).await;
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
        loop {
            tokio::select! {
                Some(chunk) = chunk_rx.recv() => {
                    self.write_chunk(&chunk).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    if self.handle_command(cmd, &mut chunk_rx).await {
                        break;
                    }
                }
                else => break,
            }
        }

        self.stop_sinks().await;
    }

    /// Handles a router command. Returns true if the router should stop.
    async fn handle_command(
        &self,
        cmd: RouterCommand,
        chunk_rx: &mut mpsc::Receiver<AudioChunk>,
    ) -> bool {
        match cmd {
            RouterCommand::Stop => {
                self.drain_remaining(chunk_rx).await;
                true
            }
        }
    }

    /// Drains remaining chunks during shutdown.
    async fn drain_remaining(&self, chunk_rx: &mut mpsc::Receiver<AudioChunk>) {
        while let Ok(chunk) = chunk_rx.try_recv() {
            self.write_chunk(&chunk).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SinkError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    struct TestSink {
        name: String,
        write_count: AtomicUsize,
        fail_count: AtomicUsize,
        stop_called: AtomicBool,
        stop_fail: AtomicBool,
        /// Tracks timestamps of chunks received for order verification.
        received_timestamps: Mutex<Vec<Duration>>,
    }

    impl TestSink {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                write_count: AtomicUsize::new(0),
                fail_count: AtomicUsize::new(0),
                stop_called: AtomicBool::new(false),
                stop_fail: AtomicBool::new(false),
                received_timestamps: Mutex::new(Vec::new()),
            }
        }

        fn failing(name: &str, fail_times: usize) -> Self {
            Self {
                name: name.to_string(),
                write_count: AtomicUsize::new(0),
                fail_count: AtomicUsize::new(fail_times),
                stop_called: AtomicBool::new(false),
                stop_fail: AtomicBool::new(false),
                received_timestamps: Mutex::new(Vec::new()),
            }
        }

        fn with_stop_failure(name: &str) -> Self {
            Self {
                name: name.to_string(),
                write_count: AtomicUsize::new(0),
                fail_count: AtomicUsize::new(0),
                stop_called: AtomicBool::new(false),
                stop_fail: AtomicBool::new(true),
                received_timestamps: Mutex::new(Vec::new()),
            }
        }

        fn writes(&self) -> usize {
            self.write_count.load(Ordering::SeqCst)
        }

        fn was_stopped(&self) -> bool {
            self.stop_called.load(Ordering::SeqCst)
        }

        fn timestamps(&self) -> Vec<Duration> {
            self.received_timestamps.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Sink for TestSink {
        fn name(&self) -> &str {
            &self.name
        }

        async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError> {
            let remaining = self.fail_count.load(Ordering::SeqCst);
            if remaining > 0 {
                self.fail_count.fetch_sub(1, Ordering::SeqCst);
                return Err(SinkError::custom("intentional failure"));
            }
            self.write_count.fetch_add(1, Ordering::SeqCst);
            self.received_timestamps
                .lock()
                .unwrap()
                .push(chunk.timestamp);
            Ok(())
        }

        async fn on_stop(&self) -> Result<(), SinkError> {
            self.stop_called.store(true, Ordering::SeqCst);
            if self.stop_fail.load(Ordering::SeqCst) {
                return Err(SinkError::custom("stop failed"));
            }
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

    /// Creates a test chunk from a specific source.
    fn chunk_from(source: &str, timestamp_ms: u64) -> AudioChunk {
        AudioChunk::with_source(
            vec![1, 2, 3],
            Duration::from_millis(timestamp_ms),
            16000,
            1,
            SourceId::new(source),
        )
    }

    #[tokio::test]
    async fn test_direct_routing_single_source() {
        let sink_mic = Arc::new(TestSink::new("mic_sink"));
        let sink_speaker = Arc::new(TestSink::new("speaker_sink"));

        let routes = vec![
            SinkRoute::Single(SourceId::new("mic")),
            SinkRoute::Single(SourceId::new("speaker")),
        ];

        let router = Router::with_routing(
            vec![sink_mic.clone(), sink_speaker.clone()],
            &routes,
            &[SourceId::new("mic"), SourceId::new("speaker")],
            StreamConfig::default(),
        )
        .unwrap();

        // Chunk from mic should only go to mic_sink
        router.write_chunk(&chunk_from("mic", 50)).await;
        assert_eq!(sink_mic.writes(), 1);
        assert_eq!(sink_speaker.writes(), 0);

        // Chunk from speaker should only go to speaker_sink
        router.write_chunk(&chunk_from("speaker", 50)).await;
        assert_eq!(sink_mic.writes(), 1);
        assert_eq!(sink_speaker.writes(), 1);
    }

    #[tokio::test]
    async fn test_broadcast_receives_all_sources() {
        let sink_broadcast = Arc::new(TestSink::new("broadcast"));
        let sink_mic_only = Arc::new(TestSink::new("mic_only"));

        let routes = vec![
            SinkRoute::Broadcast,
            SinkRoute::Single(SourceId::new("mic")),
        ];

        let router = Router::with_routing(
            vec![sink_broadcast.clone(), sink_mic_only.clone()],
            &routes,
            &[SourceId::new("mic"), SourceId::new("speaker")],
            StreamConfig::default(),
        )
        .unwrap();

        // Mic chunk: broadcast gets it, mic_only gets it
        router.write_chunk(&chunk_from("mic", 50)).await;
        assert_eq!(sink_broadcast.writes(), 1);
        assert_eq!(sink_mic_only.writes(), 1);

        // Speaker chunk: broadcast gets it, mic_only doesn't
        router.write_chunk(&chunk_from("speaker", 50)).await;
        assert_eq!(sink_broadcast.writes(), 2);
        assert_eq!(sink_mic_only.writes(), 1);
    }

    // ==================== stop_sinks() Tests ====================

    #[tokio::test]
    async fn test_stop_sinks_calls_all_sinks() {
        let sink1 = Arc::new(TestSink::new("sink1"));
        let sink2 = Arc::new(TestSink::new("sink2"));
        let sink3 = Arc::new(TestSink::new("sink3"));

        let router = Router::new(
            vec![sink1.clone(), sink2.clone(), sink3.clone()],
            StreamConfig::default(),
        );

        router.stop_sinks().await;

        assert!(sink1.was_stopped());
        assert!(sink2.was_stopped());
        assert!(sink3.was_stopped());
    }

    #[tokio::test]
    async fn test_stop_sinks_continues_after_failure() {
        let sink1 = Arc::new(TestSink::new("sink1"));
        let sink2 = Arc::new(TestSink::with_stop_failure("sink2"));
        let sink3 = Arc::new(TestSink::new("sink3"));

        let router = Router::new(
            vec![sink1.clone(), sink2.clone(), sink3.clone()],
            StreamConfig::default(),
        );

        router.stop_sinks().await;

        // All sinks should have on_stop called, even after sink2 fails
        assert!(sink1.was_stopped());
        assert!(sink2.was_stopped());
        assert!(sink3.was_stopped());
    }

    #[tokio::test]
    async fn test_stop_sinks_emits_error_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);

        let callback: EventCallback = Arc::new(move |e| {
            events_clone.lock().unwrap().push(e);
        });

        let sink1 = Arc::new(TestSink::new("sink1"));
        let sink2 = Arc::new(TestSink::with_stop_failure("failing_sink"));

        let router =
            Router::new(vec![sink1, sink2], StreamConfig::default()).with_event_callback(callback);

        router.stop_sinks().await;

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);

        if let StreamEvent::SinkError { sink_name, error } = &captured[0] {
            assert_eq!(sink_name, "failing_sink");
            assert!(error.contains("Error during shutdown:"));
        } else {
            panic!("Expected SinkError event");
        }
    }

    // ==================== drain_remaining() Tests ====================

    #[tokio::test]
    async fn test_drain_remaining_preserves_order() {
        let sink = Arc::new(TestSink::new("sink"));
        let router = Router::new(vec![sink.clone()], StreamConfig::default());

        let (chunk_tx, mut chunk_rx) = mpsc::channel(10);

        // Send chunks with distinct timestamps
        for i in 0..5 {
            let chunk = AudioChunk::new(
                vec![i as i16; 100],
                Duration::from_millis(i as u64 * 100),
                16000,
                1,
            );
            chunk_tx.send(chunk).await.unwrap();
        }
        drop(chunk_tx);

        router.drain_remaining(&mut chunk_rx).await;

        // Verify order preserved
        let timestamps = sink.timestamps();
        assert_eq!(timestamps.len(), 5);
        for i in 0..5 {
            assert_eq!(timestamps[i], Duration::from_millis(i as u64 * 100));
        }
    }

    #[tokio::test]
    async fn test_drain_remaining_empty_queue() {
        let sink = Arc::new(TestSink::new("sink"));
        let router = Router::new(vec![sink.clone()], StreamConfig::default());

        let (_tx, mut chunk_rx) = mpsc::channel::<AudioChunk>(10);
        // Don't send any chunks, just drop sender
        drop(_tx);

        // Should not panic
        router.drain_remaining(&mut chunk_rx).await;

        assert_eq!(sink.writes(), 0);
    }

    #[tokio::test]
    async fn test_drain_remaining_respects_retries() {
        let sink = Arc::new(TestSink::failing("sink", 2)); // Fails twice, then succeeds

        let mut config = StreamConfig::default();
        config.sink_retry_delay = Duration::from_millis(1);

        let router = Router::new(vec![sink.clone()], config);

        let (chunk_tx, mut chunk_rx) = mpsc::channel(10);
        chunk_tx
            .send(AudioChunk::new(vec![0; 100], Duration::ZERO, 16000, 1))
            .await
            .unwrap();
        drop(chunk_tx);

        router.drain_remaining(&mut chunk_rx).await;

        // Should have retried and succeeded
        assert_eq!(sink.writes(), 1);
    }

    #[tokio::test]
    async fn test_drain_remaining_applies_routing() {
        let sink_mic = Arc::new(TestSink::new("mic"));
        let sink_speaker = Arc::new(TestSink::new("speaker"));

        let routes = vec![
            SinkRoute::Single(SourceId::new("mic")),
            SinkRoute::Single(SourceId::new("speaker")),
        ];

        let router = Router::with_routing(
            vec![sink_mic.clone(), sink_speaker.clone()],
            &routes,
            &[SourceId::new("mic"), SourceId::new("speaker")],
            StreamConfig::default(),
        )
        .unwrap();

        let (chunk_tx, mut chunk_rx) = mpsc::channel(10);
        // Send mic chunk
        chunk_tx.send(chunk_from("mic", 0)).await.unwrap();
        drop(chunk_tx);

        router.drain_remaining(&mut chunk_rx).await;

        // Only mic sink should have received it
        assert_eq!(sink_mic.writes(), 1);
        assert_eq!(sink_speaker.writes(), 0);
    }

    // ==================== Integration Tests ====================

    #[tokio::test]
    async fn test_run_drains_before_stop() {
        let sink = Arc::new(TestSink::new("sink"));
        let router = Router::new(vec![sink.clone()], StreamConfig::default());

        let (chunk_tx, chunk_rx) = mpsc::channel(10);
        let (cmd_tx, cmd_rx) = mpsc::channel(1);

        // Send 3 chunks
        for i in 0..3 {
            chunk_tx
                .send(AudioChunk::new(
                    vec![i as i16; 100],
                    Duration::from_millis(i as u64 * 100),
                    16000,
                    1,
                ))
                .await
                .unwrap();
        }

        // Send stop command
        cmd_tx.send(RouterCommand::Stop).await.unwrap();

        // Run router
        router.run(chunk_rx, cmd_rx).await;

        // All 3 chunks should have been processed
        assert_eq!(sink.writes(), 3);
        // on_stop should have been called
        assert!(sink.was_stopped());
    }

    #[tokio::test]
    async fn test_run_stops_sinks_after_drain() {
        let sink1 = Arc::new(TestSink::new("sink1"));
        let sink2 = Arc::new(TestSink::with_stop_failure("sink2"));

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        let callback: EventCallback = Arc::new(move |e| {
            events_clone.lock().unwrap().push(e);
        });

        let router = Router::new(vec![sink1.clone(), sink2.clone()], StreamConfig::default())
            .with_event_callback(callback);

        let (_chunk_tx, chunk_rx) = mpsc::channel(10);
        let (cmd_tx, cmd_rx) = mpsc::channel(1);

        cmd_tx.send(RouterCommand::Stop).await.unwrap();

        router.run(chunk_rx, cmd_rx).await;

        // Both sinks should have on_stop called
        assert!(sink1.was_stopped());
        assert!(sink2.was_stopped());

        // Error event should have been emitted for sink2
        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
    }
}
