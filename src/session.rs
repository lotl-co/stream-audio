//! Audio capture session management.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::pipeline::RouterCommand;
use crate::source::{CaptureStream, SourceId};
use crate::{EventCallback, StreamAudioError, StreamEvent};

/// Default timeout for graceful shutdown operations.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Reason string for `SourceStopped` event when session is explicitly stopped.
const STOP_REASON_SESSION_STOPPED: &str = "session stopped";

/// Statistics about a recording session.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    /// Total chunks processed.
    pub chunks_processed: u64,
    /// Total samples captured.
    pub samples_captured: u64,
    /// Number of buffer overflows.
    pub buffer_overflows: u64,
    /// Number of audio gaps detected (suspicious zero-sample runs).
    pub audio_gaps_detected: u64,
    /// Total duration of detected gaps in milliseconds.
    pub total_gap_duration_ms: u64,
    /// Total samples that hit max amplitude (clipping indicator).
    pub clipped_samples: u64,
}

/// Internal state shared between Session and background tasks.
pub(crate) struct SessionState {
    pub running: AtomicBool,
    pub chunks_processed: AtomicU64,
    pub samples_captured: AtomicU64,
    pub buffer_overflows: AtomicU64,
    pub audio_gaps_detected: AtomicU64,
    pub total_gap_duration_ms: AtomicU64,
    pub clipped_samples: AtomicU64,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(true),
            chunks_processed: AtomicU64::new(0),
            samples_captured: AtomicU64::new(0),
            buffer_overflows: AtomicU64::new(0),
            audio_gaps_detected: AtomicU64::new(0),
            total_gap_duration_ms: AtomicU64::new(0),
            clipped_samples: AtomicU64::new(0),
        }
    }
}

/// Handle to a running audio capture session.
///
/// The `Session` is returned by [`StreamAudioBuilder::start()`] and represents
/// an active audio capture. Audio capture runs in background tasks until
/// `stop()` is called or the `Session` is dropped.
///
/// # Lifecycle
///
/// 1. Created by [`StreamAudioBuilder::start()`]
/// 2. Audio capture runs in background
/// 3. Call [`stop()`](Session::stop) for graceful shutdown
/// 4. Dropping the `Session` also stops capture (but prefer explicit `stop()`)
///
/// # Cleanup Guarantees
///
/// - **`stop()`**: Guaranteed cleanup. Waits for all tasks to complete.
/// - **`Drop`**: Best-effort only. Spawns background cleanup that may not
///   complete if the Tokio runtime is shutting down.
///
/// Always call `stop()` explicitly when cleanup must be guaranteed.
///
/// # Example
///
/// ```ignore
/// let session = StreamAudio::builder()
///     .add_sink(FileSink::wav("output.wav"))
///     .start()
///     .await?;
///
/// // Capture runs in background...
/// tokio::time::sleep(Duration::from_secs(10)).await;
///
/// // Graceful shutdown
/// session.stop().await?;
/// ```
///
/// [`StreamAudioBuilder::start()`]: crate::StreamAudioBuilder::start
pub struct Session {
    state: Arc<SessionState>,
    router_cmd_tx: mpsc::Sender<RouterCommand>,
    router_handle: Option<JoinHandle<()>>,
    /// Capture handles for all sources.
    capture_handles: Vec<JoinHandle<()>>,
    /// Keep the capture streams alive - dropping them stops CPAL.
    #[allow(dead_code)]
    capture_streams: Vec<CaptureStream>,
    /// Source IDs for event emission.
    source_ids: Vec<SourceId>,
    /// Event callback for `SourceStopped` events.
    event_callback: Option<EventCallback>,
}

impl Session {
    /// Creates a new session with capture sources.
    pub(crate) fn new(
        state: Arc<SessionState>,
        router_cmd_tx: mpsc::Sender<RouterCommand>,
        router_handle: JoinHandle<()>,
        capture_handles: Vec<JoinHandle<()>>,
        capture_streams: Vec<CaptureStream>,
        source_ids: Vec<SourceId>,
        event_callback: Option<EventCallback>,
    ) -> Self {
        Self {
            state,
            router_cmd_tx,
            router_handle: Some(router_handle),
            capture_handles,
            capture_streams,
            source_ids,
            event_callback,
        }
    }

    /// Returns `true` if the session is still running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state.running.load(Ordering::SeqCst)
    }

    /// Returns current session statistics.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            chunks_processed: self.state.chunks_processed.load(Ordering::SeqCst),
            samples_captured: self.state.samples_captured.load(Ordering::SeqCst),
            buffer_overflows: self.state.buffer_overflows.load(Ordering::SeqCst),
            audio_gaps_detected: self.state.audio_gaps_detected.load(Ordering::SeqCst),
            total_gap_duration_ms: self.state.total_gap_duration_ms.load(Ordering::SeqCst),
            clipped_samples: self.state.clipped_samples.load(Ordering::SeqCst),
        }
    }

    /// Gracefully stops the audio capture session.
    ///
    /// This will:
    /// 1. Stop the CPAL audio stream
    /// 2. Drain any remaining audio in the buffer to sinks
    /// 3. Call `on_stop()` on all sinks
    /// 4. Wait for background tasks to complete
    ///
    /// # Errors
    ///
    /// Returns an error if shutdown fails.
    pub async fn stop(mut self) -> Result<(), StreamAudioError> {
        self.stop_internal().await
    }

    async fn stop_internal(&mut self) -> Result<(), StreamAudioError> {
        if !self.state.running.swap(false, Ordering::SeqCst) {
            // Already stopped
            return Ok(());
        }

        // Send stop command to router (don't wait forever if channel is full)
        let _ = tokio::time::timeout(
            Duration::from_millis(100),
            self.router_cmd_tx.send(RouterCommand::Stop),
        )
        .await;

        // Wait for all capture tasks with timeout
        for handle in self.capture_handles.drain(..) {
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, handle)
                .await
                .is_err()
            {
                tracing::warn!("Capture task did not complete within timeout");
            }
        }

        // Wait for router task with timeout
        if let Some(handle) = self.router_handle.take() {
            if tokio::time::timeout(SHUTDOWN_TIMEOUT, handle)
                .await
                .is_err()
            {
                tracing::warn!("Router task did not complete within timeout");
            }
        }

        // Emit SourceStopped events for all sources
        if let Some(ref callback) = self.event_callback {
            for source_id in &self.source_ids {
                callback(StreamEvent::SourceStopped {
                    source_id: source_id.clone(),
                    reason: STOP_REASON_SESSION_STOPPED.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Spawns a background task to clean up resources.
    ///
    /// This is used when the Session is dropped without explicit `stop()`.
    /// The cleanup runs in a detached task so Drop doesn't block.
    ///
    /// If the Tokio runtime is shutting down, cleanup is skipped with a warning.
    fn spawn_cleanup(&mut self) {
        // Signal stop
        self.state.running.store(false, Ordering::SeqCst);

        // Try to send stop command (non-blocking)
        let _ = self.router_cmd_tx.try_send(RouterCommand::Stop);

        // Take the handles so the tasks can be cleaned up in background
        let capture_handles: Vec<_> = self.capture_handles.drain(..).collect();
        let router_handle = self.router_handle.take();

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                "Session dropped after runtime shutdown; cleanup skipped. \
                 Call stop() explicitly for guaranteed cleanup."
            );
            return;
        };

        // Spawn a detached cleanup task
        handle.spawn(async move {
            // Wait briefly for all capture tasks to finish
            for handle in capture_handles {
                if tokio::time::timeout(SHUTDOWN_TIMEOUT, handle)
                    .await
                    .is_err()
                {
                    tracing::warn!("Capture task did not complete on drop");
                }
            }

            if let Some(handle) = router_handle {
                if tokio::time::timeout(SHUTDOWN_TIMEOUT, handle)
                    .await
                    .is_err()
                {
                    tracing::warn!("Router task did not complete on drop");
                }
            }
        });
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.state.running.load(Ordering::SeqCst) {
            // Session dropped without explicit stop() - spawn background cleanup
            // This prevents blocking in Drop while still ensuring cleanup happens
            self.spawn_cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn test_session_state_new() {
        let state = SessionState::new();
        assert!(state.running.load(Ordering::SeqCst));
        assert_eq!(state.chunks_processed.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_session_stats_default() {
        let stats = SessionStats::default();
        assert_eq!(stats.chunks_processed, 0);
        assert_eq!(stats.samples_captured, 0);
        assert_eq!(stats.buffer_overflows, 0);
    }

    /// Creates a minimal test session with mock channels.
    fn create_test_session() -> (Session, mpsc::Receiver<RouterCommand>) {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(10);

        // Create a dummy router handle that completes immediately
        let router_handle = tokio::spawn(async {});

        let session = Session::new(
            state,
            router_cmd_tx,
            router_handle,
            vec![], // No capture handles
            vec![], // No capture streams
            vec![SourceId::new("test_source")],
            None,
        );

        (session, router_cmd_rx)
    }

    /// Creates a test session with an event callback that records events.
    fn create_test_session_with_callback(
        events: Arc<Mutex<Vec<StreamEvent>>>,
    ) -> (Session, mpsc::Receiver<RouterCommand>) {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(10);

        let router_handle = tokio::spawn(async {});

        let callback: EventCallback = {
            let events = Arc::clone(&events);
            Arc::new(move |e| {
                events.lock().unwrap().push(e);
            })
        };

        let session = Session::new(
            state,
            router_cmd_tx,
            router_handle,
            vec![],
            vec![],
            vec![SourceId::new("source1"), SourceId::new("source2")],
            Some(callback),
        );

        (session, router_cmd_rx)
    }

    #[test]
    fn test_session_is_running_initial_true() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (session, _rx) = create_test_session();
            assert!(session.is_running());
        });
    }

    #[tokio::test]
    async fn test_session_is_running_after_stop_false() {
        let (session, _rx) = create_test_session();
        assert!(session.is_running());
        session.stop().await.unwrap();
        // Session is consumed by stop(), can't check is_running after
        // Instead, we verify the state was set to false in the stop_internal test
    }

    #[tokio::test]
    async fn test_session_stop_sets_running_false() {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, _rx) = mpsc::channel(10);
        let router_handle = tokio::spawn(async {});

        let mut session = Session::new(
            Arc::clone(&state),
            router_cmd_tx,
            router_handle,
            vec![],
            vec![],
            vec![],
            None,
        );

        assert!(state.running.load(Ordering::SeqCst));
        session.stop_internal().await.unwrap();
        assert!(!state.running.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_session_stop_twice_is_safe() {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, _rx) = mpsc::channel(10);
        let router_handle = tokio::spawn(async {});

        let mut session = Session::new(
            Arc::clone(&state),
            router_cmd_tx,
            router_handle,
            vec![],
            vec![],
            vec![],
            None,
        );

        // First stop
        let result1 = session.stop_internal().await;
        assert!(result1.is_ok());

        // Second stop should also succeed (idempotent)
        let result2 = session.stop_internal().await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn test_session_stop_sends_router_command() {
        let (session, mut rx) = create_test_session();

        session.stop().await.unwrap();

        // Verify stop command was sent
        let cmd = rx.try_recv();
        assert!(matches!(cmd, Ok(RouterCommand::Stop)));
    }

    #[test]
    fn test_session_stats_reflects_state() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let state = Arc::new(SessionState::new());
            state.chunks_processed.store(100, Ordering::SeqCst);
            state.samples_captured.store(160000, Ordering::SeqCst);
            state.buffer_overflows.store(2, Ordering::SeqCst);
            state.audio_gaps_detected.store(3, Ordering::SeqCst);
            state.total_gap_duration_ms.store(150, Ordering::SeqCst);
            state.clipped_samples.store(50, Ordering::SeqCst);

            let (router_cmd_tx, _rx) = mpsc::channel(10);
            let router_handle = tokio::spawn(async {});

            let session = Session::new(
                state,
                router_cmd_tx,
                router_handle,
                vec![],
                vec![],
                vec![],
                None,
            );

            let stats = session.stats();
            assert_eq!(stats.chunks_processed, 100);
            assert_eq!(stats.samples_captured, 160000);
            assert_eq!(stats.buffer_overflows, 2);
            assert_eq!(stats.audio_gaps_detected, 3);
            assert_eq!(stats.total_gap_duration_ms, 150);
            assert_eq!(stats.clipped_samples, 50);
        });
    }

    #[tokio::test]
    async fn test_session_stop_emits_source_stopped_events() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (session, _rx) = create_test_session_with_callback(Arc::clone(&events));

        session.stop().await.unwrap();

        let captured_events = events.lock().unwrap();
        assert_eq!(captured_events.len(), 2); // Two sources

        // Check first event
        if let StreamEvent::SourceStopped { source_id, reason } = &captured_events[0] {
            assert_eq!(source_id.as_str(), "source1");
            assert_eq!(reason, STOP_REASON_SESSION_STOPPED);
        } else {
            panic!("Expected SourceStopped event");
        }

        // Check second event
        if let StreamEvent::SourceStopped { source_id, reason } = &captured_events[1] {
            assert_eq!(source_id.as_str(), "source2");
            assert_eq!(reason, STOP_REASON_SESSION_STOPPED);
        } else {
            panic!("Expected SourceStopped event");
        }
    }

    #[tokio::test]
    async fn test_session_drop_without_stop_triggers_cleanup() {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, mut rx) = mpsc::channel(10);
        let router_handle = tokio::spawn(async {
            // Simulate a router that waits briefly
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        {
            let _session = Session::new(
                Arc::clone(&state),
                router_cmd_tx,
                router_handle,
                vec![],
                vec![],
                vec![],
                None,
            );
            assert!(state.running.load(Ordering::SeqCst));
            // Session dropped here without explicit stop()
        }

        // Give cleanup task a moment to run
        tokio::time::sleep(Duration::from_millis(10)).await;

        // State should be set to not running
        assert!(!state.running.load(Ordering::SeqCst));

        // Stop command should have been sent
        let cmd = rx.try_recv();
        assert!(matches!(cmd, Ok(RouterCommand::Stop)));
    }

    #[tokio::test]
    async fn test_session_drop_after_stop_no_double_cleanup() {
        let state = Arc::new(SessionState::new());
        let (router_cmd_tx, mut rx) = mpsc::channel(10);
        let router_handle = tokio::spawn(async {});

        {
            let session = Session::new(
                Arc::clone(&state),
                router_cmd_tx,
                router_handle,
                vec![],
                vec![],
                vec![],
                None,
            );

            // Explicit stop
            session.stop().await.unwrap();
        }

        // Only one Stop command should have been sent
        assert!(matches!(rx.try_recv(), Ok(RouterCommand::Stop)));
        assert!(rx.try_recv().is_err()); // No second Stop command
    }
}
