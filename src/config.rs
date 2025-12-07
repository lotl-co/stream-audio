//! Configuration types for audio streams.

use std::time::Duration;

/// Preset audio formats for common use cases.
///
/// These presets configure sample rate and channel count for typical scenarios.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormatPreset {
    /// 16kHz mono - optimal for speech-to-text services.
    ///
    /// Most transcription APIs prefer this format for efficiency and accuracy.
    #[default]
    Transcription,

    /// Use the device's native format without conversion.
    ///
    /// Avoids resampling overhead but may not be compatible with all sinks.
    Native,
}

impl FormatPreset {
    /// Returns the target sample rate for this preset, or `None` for native.
    #[must_use]
    pub fn sample_rate(&self) -> Option<u32> {
        match self {
            Self::Transcription => Some(16000),
            Self::Native => None,
        }
    }

    /// Returns the target channel count for this preset, or `None` for native.
    #[must_use]
    pub fn channels(&self) -> Option<u16> {
        match self {
            Self::Transcription => Some(1),
            Self::Native => None,
        }
    }
}

/// Configuration for stream behavior.
///
/// Use [`StreamConfig::default()`] for sensible defaults, or customize as needed.
///
/// # Example
///
/// ```
/// use stream_audio::StreamConfig;
/// use std::time::Duration;
///
/// let config = StreamConfig {
///     chunk_duration: Duration::from_millis(50),
///     ..Default::default()
/// };
/// ```
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// Duration of each audio chunk sent to sinks.
    ///
    /// Smaller values reduce latency but increase overhead.
    /// Default: 100ms
    pub chunk_duration: Duration,

    /// Size of the ring buffer for resilience.
    ///
    /// This buffer absorbs pressure from slow sinks. If it fills,
    /// oldest audio is dropped and a [`StreamEvent::BufferOverflow`] is emitted.
    /// Default: 30 seconds
    ///
    /// [`StreamEvent::BufferOverflow`]: crate::StreamEvent::BufferOverflow
    pub ring_buffer_duration: Duration,

    /// Number of retry attempts for failed sink writes.
    ///
    /// Default: 3
    pub sink_retry_attempts: u32,

    /// Initial delay between sink retry attempts.
    ///
    /// Uses exponential backoff (delay doubles each attempt).
    /// Default: 100ms
    pub sink_retry_delay: Duration,

    /// Duration of merge windows for multi-source capture.
    ///
    /// When multiple sources are merged, chunks are aligned by timestamp
    /// into windows of this duration. Should typically match `chunk_duration`.
    /// Default: 100ms
    pub merge_window_duration: Duration,

    /// Timeout for incomplete merge windows.
    ///
    /// If a merge window doesn't receive chunks from all sources within
    /// this duration, it will be emitted with available sources only
    /// (missing sources filled with silence) and a [`StreamEvent::MergeIncomplete`]
    /// event is emitted.
    /// Default: 200ms
    ///
    /// [`StreamEvent::MergeIncomplete`]: crate::StreamEvent::MergeIncomplete
    pub merge_window_timeout: Duration,

    /// Maximum pending merge windows before oldest is evicted.
    ///
    /// Limits merge backlog to N × `merge_window_duration`. Increase if sources
    /// have significant clock drift. Evicted windows emit with available data
    /// and a [`StreamEvent::MergeIncomplete`] event.
    /// Default: 10 (~1 second at 100ms windows)
    ///
    /// [`StreamEvent::MergeIncomplete`]: crate::StreamEvent::MergeIncomplete
    pub max_pending_windows: usize,
}

/// Default maximum pending windows (~1 second at 100ms windows).
const DEFAULT_MAX_PENDING_WINDOWS: usize = 10;

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            chunk_duration: Duration::from_millis(100),
            ring_buffer_duration: Duration::from_secs(30),
            sink_retry_attempts: 3,
            sink_retry_delay: Duration::from_millis(100),
            merge_window_duration: Duration::from_millis(100),
            merge_window_timeout: Duration::from_millis(200),
            max_pending_windows: DEFAULT_MAX_PENDING_WINDOWS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_preset_transcription() {
        let preset = FormatPreset::Transcription;
        assert_eq!(preset.sample_rate(), Some(16000));
        assert_eq!(preset.channels(), Some(1));
    }

    #[test]
    fn test_format_preset_native() {
        let preset = FormatPreset::Native;
        assert_eq!(preset.sample_rate(), None);
        assert_eq!(preset.channels(), None);
    }

    #[test]
    fn test_format_preset_default() {
        assert_eq!(FormatPreset::default(), FormatPreset::Transcription);
    }

    #[test]
    fn test_stream_config_defaults() {
        let config = StreamConfig::default();
        assert_eq!(config.chunk_duration, Duration::from_millis(100));
        assert_eq!(config.ring_buffer_duration, Duration::from_secs(30));
        assert_eq!(config.sink_retry_attempts, 3);
        assert_eq!(config.sink_retry_delay, Duration::from_millis(100));
        assert_eq!(config.merge_window_duration, Duration::from_millis(100));
        assert_eq!(config.merge_window_timeout, Duration::from_millis(200));
        assert_eq!(config.max_pending_windows, 10);
    }
}
