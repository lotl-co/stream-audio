//! Audio capture session management.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::pipeline::RouterCommand;
use crate::source::CaptureStream;
use crate::StreamAudioError;

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
    capture_handle: Option<JoinHandle<()>>,
    // Keep the capture stream alive - dropping it stops CPAL
    #[allow(dead_code)]
    capture_stream: CaptureStream,
}

impl Session {
    /// Creates a new session with the given handles.
    pub(crate) fn new(
        state: Arc<SessionState>,
        router_cmd_tx: mpsc::Sender<RouterCommand>,
        router_handle: JoinHandle<()>,
        capture_handle: JoinHandle<()>,
        capture_stream: CaptureStream,
    ) -> Self {
        Self {
            state,
            router_cmd_tx,
            router_handle: Some(router_handle),
            capture_handle: Some(capture_handle),
            capture_stream,
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

        // Send stop command to router
        let _ = self.router_cmd_tx.send(RouterCommand::Stop).await;

        // Wait for capture task to finish
        if let Some(handle) = self.capture_handle.take() {
            let _ = handle.await;
        }

        // Wait for router task to finish
        if let Some(handle) = self.router_handle.take() {
            let _ = handle.await;
        }

        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.state.running.load(Ordering::SeqCst) {
            // Session dropped without explicit stop() - trigger background cleanup
            self.state.running.store(false, Ordering::SeqCst);
            let _ = self.router_cmd_tx.try_send(RouterCommand::Stop);
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
