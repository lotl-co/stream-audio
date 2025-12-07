//! Audio data chunk with metadata.

use std::sync::Arc;
use std::time::Duration;

use crate::source::SourceId;

/// A discrete buffer of audio samples with associated metadata.
///
/// `AudioChunk` is the fundamental unit of audio data passed through the pipeline.
/// Each chunk contains PCM samples along with timing and format information.
///
/// Samples are stored in an `Arc<Vec<i16>>` for efficient zero-copy sharing
/// between multiple sinks.
///
/// # Example
///
/// ```
/// use stream_audio::AudioChunk;
/// use std::time::Duration;
///
/// let chunk = AudioChunk::new(vec![0i16; 1600], Duration::from_millis(0), 16000, 1);
/// assert_eq!(chunk.duration(), Duration::from_millis(100));
///
/// // Samples are Arc-wrapped for efficient sharing
/// let chunk2 = chunk.clone();  // Cheap clone - shares sample data
/// ```
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// PCM audio samples in 16-bit signed integer format.
    ///
    /// Wrapped in `Arc` for zero-copy sharing between sinks.
    pub samples: Arc<Vec<i16>>,

    /// Timestamp from the start of the recording session.
    pub timestamp: Duration,

    /// Sample rate in Hz (e.g., 16000, 44100, 48000).
    pub sample_rate: u32,

    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u16,

    /// Source identifier for multi-source capture.
    ///
    /// `None` for merged chunks or single-source capture (backward compatibility).
    pub source_id: Option<SourceId>,
}

impl AudioChunk {
    /// Creates a new `AudioChunk` with the given parameters.
    ///
    /// For single-source capture, `source_id` is set to `None`.
    pub fn new(samples: Vec<i16>, timestamp: Duration, sample_rate: u32, channels: u16) -> Self {
        Self {
            samples: Arc::new(samples),
            timestamp,
            sample_rate,
            channels,
            source_id: None,
        }
    }

    /// Creates a new `AudioChunk` with a source identifier.
    ///
    /// Used in multi-source capture to identify which device produced the audio.
    pub fn with_source(
        samples: Vec<i16>,
        timestamp: Duration,
        sample_rate: u32,
        channels: u16,
        source_id: SourceId,
    ) -> Self {
        Self {
            samples: Arc::new(samples),
            timestamp,
            sample_rate,
            channels,
            source_id: Some(source_id),
        }
    }

    /// Creates a new `AudioChunk` from pre-wrapped Arc samples.
    ///
    /// Useful when creating merged chunks or when samples are already Arc-wrapped.
    pub fn from_arc(
        samples: Arc<Vec<i16>>,
        timestamp: Duration,
        sample_rate: u32,
        channels: u16,
        source_id: Option<SourceId>,
    ) -> Self {
        Self {
            samples,
            timestamp,
            sample_rate,
            channels,
            source_id,
        }
    }

    /// Returns the duration of this audio chunk.
    ///
    /// Calculated from the number of samples, sample rate, and channel count.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 || self.channels == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples.len() / self.channels as usize;
        Duration::from_secs_f64(frames as f64 / self.sample_rate as f64)
    }

    /// Returns the number of audio frames in this chunk.
    ///
    /// A frame contains one sample per channel.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / self.channels as usize
    }

    /// Returns `true` if this chunk contains no samples.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_mono_16khz() {
        let chunk = AudioChunk::new(vec![0i16; 1600], Duration::ZERO, 16000, 1);
        assert_eq!(chunk.duration(), Duration::from_millis(100));
    }

    #[test]
    fn test_duration_stereo_48khz() {
        let chunk = AudioChunk::new(vec![0i16; 9600], Duration::ZERO, 48000, 2);
        // 9600 samples / 2 channels = 4800 frames / 48000 Hz = 100ms
        assert_eq!(chunk.duration(), Duration::from_millis(100));
    }

    #[test]
    fn test_frame_count() {
        let chunk = AudioChunk::new(vec![0i16; 200], Duration::ZERO, 16000, 2);
        assert_eq!(chunk.frame_count(), 100);
    }

    #[test]
    fn test_empty_chunk() {
        let chunk = AudioChunk::new(vec![], Duration::ZERO, 16000, 1);
        assert!(chunk.is_empty());
        assert_eq!(chunk.frame_count(), 0);
        assert_eq!(chunk.duration(), Duration::ZERO);
    }

    #[test]
    fn test_zero_sample_rate() {
        let chunk = AudioChunk::new(vec![0i16; 100], Duration::ZERO, 0, 1);
        assert_eq!(chunk.duration(), Duration::ZERO);
    }

    #[test]
    fn test_zero_channels() {
        let chunk = AudioChunk::new(vec![0i16; 100], Duration::ZERO, 16000, 0);
        assert_eq!(chunk.duration(), Duration::ZERO);
        assert_eq!(chunk.frame_count(), 0);
    }
}
