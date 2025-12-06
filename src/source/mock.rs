//! Mock audio source for testing without hardware.

use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;
use std::time::Duration;

/// A mock audio source that generates synthetic audio for testing.
///
/// This allows testing the full pipeline without requiring actual
/// audio hardware, making it suitable for CI environments.
///
/// # Example
///
/// ```
/// use stream_audio::source::MockSource;
///
/// let mut mock = MockSource::new(16000, 1);
///
/// // Generate 100ms of silence
/// mock.generate_silence(100);
///
/// // Generate 100ms of a 440Hz sine wave
/// mock.generate_sine(440.0, 100);
///
/// // Get the generated samples
/// let samples = mock.take_samples();
/// ```
pub struct MockSource {
    sample_rate: u32,
    channels: u16,
    samples: Vec<i16>,
}

impl MockSource {
    /// Creates a new mock source with the given format.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            samples: Vec::new(),
        }
    }

    /// Creates a mock source configured for transcription (16kHz mono).
    pub fn transcription() -> Self {
        Self::new(16000, 1)
    }

    /// Returns the sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Generates silence for the given duration in milliseconds.
    pub fn generate_silence(&mut self, duration_ms: u64) {
        let num_samples = self.samples_for_duration(duration_ms);
        self.samples
            .extend(std::iter::repeat(0i16).take(num_samples));
    }

    /// Generates a sine wave at the given frequency for the given duration.
    pub fn generate_sine(&mut self, frequency: f64, duration_ms: u64) {
        let num_frames = self.samples_for_duration(duration_ms) / self.channels as usize;
        let sample_rate = f64::from(self.sample_rate);

        for i in 0..num_frames {
            let t = i as f64 / sample_rate;
            let value = (2.0 * std::f64::consts::PI * frequency * t).sin();
            let sample = (value * 32767.0) as i16;

            // Write same sample to all channels
            for _ in 0..self.channels {
                self.samples.push(sample);
            }
        }
    }

    /// Generates white noise for the given duration.
    pub fn generate_noise(&mut self, duration_ms: u64, amplitude: f64) {
        let num_samples = self.samples_for_duration(duration_ms);
        let amplitude = (amplitude * 32767.0) as i16;

        // Simple LCG for deterministic "random" noise
        let mut seed: u32 = 12345;
        for _ in 0..num_samples {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let random = ((seed >> 16) as i32 - 32768) as i16;
            let sample = (i32::from(random) * i32::from(amplitude) / 32767) as i16;
            self.samples.push(sample);
        }
    }

    /// Adds raw samples directly.
    pub fn add_samples(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
    }

    /// Takes all accumulated samples, clearing the internal buffer.
    pub fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }

    /// Returns a reference to the accumulated samples.
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// Returns the duration of accumulated samples.
    pub fn duration(&self) -> Duration {
        let frames = self.samples.len() / self.channels as usize;
        Duration::from_secs_f64(frames as f64 / f64::from(self.sample_rate))
    }

    /// Creates a ring buffer consumer filled with the accumulated samples.
    ///
    /// This is useful for testing the pipeline with mock data.
    pub fn into_ring_buffer(self) -> ringbuf::HeapCons<i16> {
        let capacity = self.samples.len().max(1024);
        let ring_buffer = HeapRb::<i16>::new(capacity);
        let (mut producer, consumer) = ring_buffer.split();

        for sample in self.samples {
            let _ = producer.try_push(sample);
        }

        consumer
    }

    fn samples_for_duration(&self, duration_ms: u64) -> usize {
        let frames = (self.sample_rate as u64 * duration_ms / 1000) as usize;
        frames * self.channels as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Consumer;

    #[test]
    fn test_mock_source_silence() {
        let mut mock = MockSource::new(16000, 1);
        mock.generate_silence(100);

        let samples = mock.take_samples();
        assert_eq!(samples.len(), 1600); // 16000 * 0.1 = 1600
        assert!(samples.iter().all(|&s| s == 0));
    }

    #[test]
    fn test_mock_source_sine() {
        let mut mock = MockSource::new(16000, 1);
        mock.generate_sine(440.0, 100);

        let samples = mock.take_samples();
        assert_eq!(samples.len(), 1600);

        // Sine wave should have positive and negative values
        assert!(samples.iter().any(|&s| s > 0));
        assert!(samples.iter().any(|&s| s < 0));
    }

    #[test]
    fn test_mock_source_stereo() {
        let mut mock = MockSource::new(48000, 2);
        mock.generate_silence(100);

        let samples = mock.take_samples();
        // 48000 * 0.1 * 2 channels = 9600
        assert_eq!(samples.len(), 9600);
    }

    #[test]
    fn test_mock_source_duration() {
        let mut mock = MockSource::transcription();
        mock.generate_silence(500);

        assert_eq!(mock.duration(), Duration::from_millis(500));
    }

    #[test]
    fn test_mock_source_ring_buffer() {
        let mut mock = MockSource::new(16000, 1);
        mock.add_samples(&[1, 2, 3, 4, 5]);

        let mut consumer = mock.into_ring_buffer();

        let mut output = Vec::new();
        while let Some(sample) = consumer.try_pop() {
            output.push(sample);
        }

        assert_eq!(output, vec![1, 2, 3, 4, 5]);
    }
}
