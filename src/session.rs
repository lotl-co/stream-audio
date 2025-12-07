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

/// Statistics about a recording session.
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    /// Total chunks processed.
    pub chunks_processed: u64,
    /// Total samples captured.
    pub samples_captured: u64,
    /// Number of buffer overflows.
    pub buffer_overflows: u64,
}

/// Internal state shared between Session and background tasks.
pub(crate) struct SessionState {
    pub running: AtomicBool,
    pub chunks_processed: AtomicU64,
    pub samples_captured: AtomicU64,
    pub buffer_overflows: AtomicU64,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(true),
            chunks_processed: AtomicU64::new(0),
            samples_captured: AtomicU64::new(0),
            buffer_overflows: AtomicU64::new(0),
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
    pub(crate) fn new_multi(
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
    pub fn is_running(&self) -> bool {
        self.state.running.load(Ordering::SeqCst)
    }

    /// Returns current session statistics.
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            chunks_processed: self.state.chunks_processed.load(Ordering::SeqCst),
            samples_captured: self.state.samples_captured.load(Ordering::SeqCst),
            buffer_overflows: self.state.buffer_overflows.load(Ordering::SeqCst),
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
                    reason: "session stopped".to_string(),
                });
            }
        }

        Ok(())
    }

    /// Spawns a background task to clean up resources.
    ///
    /// This is used when the Session is dropped without explicit `stop()`.
    /// The cleanup runs in a detached task so Drop doesn't block.
    fn spawn_cleanup(&mut self) {
        // Signal stop
        self.state.running.store(false, Ordering::SeqCst);

        // Try to send stop command (non-blocking)
        let _ = self.router_cmd_tx.try_send(RouterCommand::Stop);

        // Take the handles so the tasks can be cleaned up in background
        let capture_handles: Vec<_> = self.capture_handles.drain(..).collect();
        let router_handle = self.router_handle.take();

        // Spawn a detached cleanup task
        tokio::spawn(async move {
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
}
