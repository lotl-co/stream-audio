//! Mock system audio backend for testing.
//!
//! Provides a fake system audio source that generates test signals
//! for unit testing without requiring actual audio hardware or permissions.

use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;

use super::{SystemAudioBackend, DEFAULT_SYSTEM_AUDIO_CHANNELS, DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE};

/// Ring buffer capacity for mock system audio (30 seconds at 48kHz stereo).
const MOCK_BUFFER_CAPACITY: usize = 48000 * 2 * 30;
use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Mock system audio backend for testing.
///
/// Generates a silent audio stream or pre-filled test data.
pub struct MockSystemAudioBackend {
    sample_rate: u32,
    channels: u16,
    /// Pre-filled samples to push to the ring buffer on start.
    prefill_samples: Vec<i16>,
}

impl MockSystemAudioBackend {
    /// Create a mock backend with default configuration.
    pub fn new() -> Self {
        Self {
            sample_rate: DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE,
            channels: DEFAULT_SYSTEM_AUDIO_CHANNELS,
            prefill_samples: Vec::new(),
        }
    }

    /// Create a mock backend with specific configuration.
    pub fn with_config(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            prefill_samples: Vec::new(),
        }
    }

    /// Create a mock backend with pre-filled test samples.
    pub fn with_samples(samples: Vec<i16>) -> Self {
        Self {
            sample_rate: DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE,
            channels: DEFAULT_SYSTEM_AUDIO_CHANNELS,
            prefill_samples: samples,
        }
    }
}

impl Default for MockSystemAudioBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemAudioBackend for MockSystemAudioBackend {
    fn start_capture(self: Box<Self>) -> Result<(CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        let ring_buffer = HeapRb::<i16>::new(MOCK_BUFFER_CAPACITY);
        let (mut producer, consumer) = ring_buffer.split();

        // Pre-fill with test samples if provided
        for &sample in &self.prefill_samples {
            let _ = producer.try_push(sample);
        }

        // Create a mock capture stream that does nothing when dropped
        let capture_stream = MockCaptureStream { _running: true };

        #[cfg(all(target_os = "macos", feature = "sck-native"))]
        {
            Ok((CaptureStream::from_system_audio(capture_stream), consumer))
        }
        #[cfg(not(all(target_os = "macos", feature = "sck-native")))]
        {
            // When sck-native feature is not enabled, we can't create a CaptureStream
            // from system audio. This is a test-only path that shouldn't happen in practice.
            let _ = capture_stream;
            Err(StreamAudioError::SystemAudioUnavailable {
                reason: "mock backend requires sck-native feature".to_string(),
            })
        }
    }

    fn native_config(&self) -> (u32, u16) {
        (self.sample_rate, self.channels)
    }

    fn name(&self) -> &'static str {
        "MockSystemAudio"
    }
}

/// Mock capture stream that does nothing.
struct MockCaptureStream {
    _running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_backend_default() {
        let backend = MockSystemAudioBackend::new();
        let (sample_rate, channels) = backend.native_config();
        assert_eq!(sample_rate, DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE);
        assert_eq!(channels, DEFAULT_SYSTEM_AUDIO_CHANNELS);
        assert_eq!(backend.name(), "MockSystemAudio");
    }

    #[test]
    fn test_mock_backend_custom_config() {
        let backend = MockSystemAudioBackend::with_config(44100, 1);
        let (sample_rate, channels) = backend.native_config();
        assert_eq!(sample_rate, 44100);
        assert_eq!(channels, 1);
    }

    #[test]
    fn test_mock_backend_with_samples() {
        let samples = vec![100, 200, 300, 400];
        let backend = Box::new(MockSystemAudioBackend::with_samples(samples.clone()));

        // Need sck-native feature to actually start capture
        #[cfg(all(target_os = "macos", feature = "sck-native"))]
        {
            let (_stream, mut consumer) = backend.start_capture().unwrap();

            // Verify pre-filled samples
            use ringbuf::traits::Consumer;
            for expected in samples {
                let actual = consumer.try_pop();
                assert_eq!(actual, Some(expected));
            }
        }
    }
}
