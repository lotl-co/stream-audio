//! Runtime events for monitoring stream health.
//!
//! Events are non-fatal notifications about stream behavior. The stream
//! continues running after events are emitted - they're for logging/metrics,
//! not error handling.

use std::sync::Arc;

use crate::source::SourceId;

/// Runtime events emitted during audio capture.
///
/// These are informational events, not errors. The stream continues
/// running after any event is emitted. Use the [`EventCallback`] to
/// log these or update metrics.
///
/// # Example
///
/// ```
/// use stream_audio::StreamEvent;
///
/// fn handle_event(event: StreamEvent) {
///     match event {
///         StreamEvent::BufferOverflow { dropped_ms } => {
///             eprintln!("Warning: dropped {}ms of audio", dropped_ms);
///         }
///         StreamEvent::SinkError { sink_name, error } => {
///             eprintln!("Sink '{}' error: {}", sink_name, error);
///         }
///         StreamEvent::StreamInterrupted { reason } => {
///             eprintln!("Stream interrupted: {}", reason);
///         }
///         StreamEvent::SourceStarted { source_id } => {
///             eprintln!("Source started: {}", source_id);
///         }
///         StreamEvent::SourceStopped { source_id, reason } => {
///             eprintln!("Source {} stopped: {}", source_id, reason);
///         }
///         StreamEvent::AudioConfigChanged { source_id, previous, current, message } => {
///             eprintln!("Config changed for {}: {:?} -> {:?} ({})", source_id, previous, current, message);
///         }
///         StreamEvent::AudioFlowStopped { source_id, silent_duration_ms } => {
///             eprintln!("Audio stopped for {}: silent for {}ms", source_id, silent_duration_ms);
///         }
///         StreamEvent::AudioFlowResumed { source_id } => {
///             eprintln!("Audio resumed for {}", source_id);
///         }
///         StreamEvent::AudioGapDetected { source_id, gap_duration_ms, cumulative_gaps, .. } => {
///             eprintln!("Audio gap detected for {}: {}ms (total gaps: {})", source_id, gap_duration_ms, cumulative_gaps);
///         }
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// The ring buffer dropped audio because sinks couldn't keep up.
    ///
    /// This happens when all sinks are slower than real-time for an
    /// extended period. Consider increasing `ring_buffer_duration` or
    /// optimizing sink performance.
    BufferOverflow {
        /// Approximate duration of audio that was dropped.
        dropped_ms: u64,
    },

    /// A sink encountered an error during write.
    ///
    /// The router will retry according to [`StreamConfig`](crate::StreamConfig)
    /// settings. If retries are exhausted, the sink may be disabled.
    SinkError {
        /// Name of the sink that errored.
        sink_name: String,
        /// Description of the error.
        error: String,
    },

    /// The audio stream was temporarily interrupted.
    ///
    /// This can happen due to OS preemption, device switching, or other
    /// transient issues. Capture typically resumes automatically.
    StreamInterrupted {
        /// Description of why the stream was interrupted.
        reason: String,
    },

    /// A source started producing audio (multi-source mode).
    SourceStarted {
        /// ID of the source that started.
        source_id: SourceId,
    },

    /// A source stopped producing audio (multi-source mode).
    ///
    /// This can happen due to device disconnection, errors, or explicit stop.
    SourceStopped {
        /// ID of the source that stopped.
        source_id: SourceId,
        /// Why the source stopped.
        reason: String,
    },

    /// Audio device configuration changed during capture.
    ///
    /// This typically happens with Bluetooth devices when activating the
    /// microphone causes a profile switch (e.g., A2DP to HFP), reducing
    /// audio quality from 44.1kHz stereo to 16kHz mono.
    ///
    /// The capture adapts automatically to the new configuration.
    AudioConfigChanged {
        /// Source affected by the config change.
        source_id: SourceId,
        /// Previous configuration (`sample_rate`, `channels`).
        previous: (u32, u16),
        /// Current configuration (`sample_rate`, `channels`).
        current: (u32, u16),
        /// Human-readable explanation of the change.
        message: String,
    },

    /// Audio flow stopped - no samples received for an extended period.
    ///
    /// This can indicate device disconnection, permission issues, or
    /// system audio configuration problems. The capture continues
    /// running and will emit [`AudioFlowResumed`] if audio returns.
    ///
    /// [`AudioFlowResumed`]: StreamEvent::AudioFlowResumed
    AudioFlowStopped {
        /// Source that stopped producing audio.
        source_id: SourceId,
        /// Duration of silence before this event was emitted.
        silent_duration_ms: u64,
    },

    /// Audio flow resumed after being stopped.
    ///
    /// Emitted when audio starts flowing again after an
    /// [`AudioFlowStopped`] event.
    ///
    /// [`AudioFlowStopped`]: StreamEvent::AudioFlowStopped
    AudioFlowResumed {
        /// Source that resumed producing audio.
        source_id: SourceId,
    },

    /// Detected a suspicious gap of zero samples in the audio stream.
    ///
    /// This indicates the audio device delivered silence instead of real audio,
    /// which typically happens when:
    /// - Another application is competing for the same audio device
    /// - USB bandwidth issues or device buffer underruns
    /// - Driver or OS audio subsystem problems
    ///
    /// The capture continues running - this is informational for quality monitoring.
    /// Downstream applications can decide how to respond (warn user, abort, etc.).
    AudioGapDetected {
        /// Source where the gap was detected.
        source_id: SourceId,
        /// Duration of this gap in milliseconds.
        gap_duration_ms: u64,
        /// Number of zero samples in this gap (for pattern analysis).
        gap_samples: u32,
        /// Position in the recording where the gap occurred.
        position: std::time::Duration,
        /// Total number of gaps detected so far in this session.
        cumulative_gaps: u32,
        /// Total gap duration in milliseconds across all gaps.
        cumulative_gap_ms: u64,
    },
}

/// Callback type for receiving runtime events.
///
/// Register an event callback via [`StreamAudioBuilder::on_event()`] to
/// receive notifications about buffer overflow, sink errors, and stream
/// interruptions.
///
/// [`StreamAudioBuilder::on_event()`]: crate::StreamAudioBuilder::on_event
///
/// # Example
///
/// ```ignore
/// use stream_audio::{StreamAudio, StreamEvent};
///
/// let session = StreamAudio::builder()
///     .on_event(|event| {
///         tracing::warn!(?event, "stream event");
///     })
///     .start()
///     .await?;
/// ```
pub type EventCallback = Arc<dyn Fn(StreamEvent) + Send + Sync>;

/// Creates an [`EventCallback`] from a closure.
///
/// This is a convenience function for creating event callbacks without
/// manually wrapping in `Arc`.
///
/// # Example
///
/// ```
/// use stream_audio::{event_callback, StreamEvent};
///
/// let callback = event_callback(|event| {
///     println!("Got event: {:?}", event);
/// });
/// ```
pub fn event_callback<F>(f: F) -> EventCallback
where
    F: Fn(StreamEvent) + Send + Sync + 'static,
{
    Arc::new(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_event_debug() {
        let event = StreamEvent::BufferOverflow { dropped_ms: 100 };
        let debug = format!("{:?}", event);
        assert!(debug.contains("BufferOverflow"));
        assert!(debug.contains("100"));
    }

    #[test]
    fn test_stream_event_clone() {
        let event = StreamEvent::SinkError {
            sink_name: "file".to_string(),
            error: "disk full".to_string(),
        };
        let cloned = event.clone();
        if let StreamEvent::SinkError { sink_name, error } = cloned {
            assert_eq!(sink_name, "file");
            assert_eq!(error, "disk full");
        } else {
            panic!("Expected SinkError variant");
        }
    }

    #[test]
    fn test_event_callback_helper() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let called = Arc::new(AtomicBool::new(false));
        let called_clone = called.clone();

        let callback = event_callback(move |_| {
            called_clone.store(true, Ordering::SeqCst);
        });

        callback(StreamEvent::BufferOverflow { dropped_ms: 0 });
        assert!(called.load(Ordering::SeqCst));
    }
}
