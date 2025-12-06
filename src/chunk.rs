//! Audio data chunk with metadata.

use std::time::Duration;

/// A discrete buffer of audio samples with associated metadata.
///
/// `AudioChunk` is the fundamental unit of audio data passed through the pipeline.
/// Each chunk contains PCM samples along with timing and format information.
///
/// # Example
///
/// ```
/// use stream_audio::AudioChunk;
/// use std::time::Duration;
///
/// let chunk = AudioChunk {
///     samples: vec![0i16; 1600],  // 100ms at 16kHz
///     timestamp: Duration::from_millis(0),
///     sample_rate: 16000,
///     channels: 1,
/// };
///
/// assert_eq!(chunk.duration(), Duration::from_millis(100));
/// ```
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// PCM audio samples in 16-bit signed integer format.
    pub samples: Vec<i16>,

    /// Timestamp from the start of the recording session.
    pub timestamp: Duration,

    /// Sample rate in Hz (e.g., 16000, 44100, 48000).
    pub sample_rate: u32,

    /// Number of audio channels (1 = mono, 2 = stereo).
    pub channels: u16,
}

impl AudioChunk {
    /// Creates a new `AudioChunk` with the given parameters.
    pub fn new(samples: Vec<i16>, timestamp: Duration, sample_rate: u32, channels: u16) -> Self {
        Self {
            samples,
            timestamp,
            sample_rate,
            channels,
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
