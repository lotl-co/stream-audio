//! Ring buffer wrapper for audio capture.

use ringbuf::traits::{Consumer, Observer, Split};
use ringbuf::HeapRb;
use std::time::Duration;

use crate::AudioChunk;

/// A ring buffer for audio samples with chunk-based reading.
///
/// This wraps the low-level ring buffer to provide chunk-based access
/// suitable for the router task.
pub struct AudioBuffer {
    consumer: ringbuf::HeapCons<i16>,
    sample_rate: u32,
    channels: u16,
    chunk_size: usize,
    samples_read: u64,
}

impl AudioBuffer {
    /// Creates a new audio buffer from a ring buffer consumer.
    pub fn new(
        consumer: ringbuf::HeapCons<i16>,
        sample_rate: u32,
        channels: u16,
        chunk_duration: Duration,
    ) -> Self {
        let frames_per_chunk = (sample_rate as f64 * chunk_duration.as_secs_f64()) as usize;
        let chunk_size = frames_per_chunk * channels as usize;

        Self {
            consumer,
            sample_rate,
            channels,
            chunk_size,
            samples_read: 0,
        }
    }

    /// Attempts to read a complete chunk from the buffer.
    ///
    /// Returns `None` if not enough samples are available.
    pub fn try_read_chunk(&mut self) -> Option<AudioChunk> {
        if self.consumer.occupied_len() < self.chunk_size {
            return None;
        }

        let mut samples = Vec::with_capacity(self.chunk_size);
        for _ in 0..self.chunk_size {
            if let Some(sample) = self.consumer.try_pop() {
                samples.push(sample);
            } else {
                break;
            }
        }

        if samples.is_empty() {
            return None;
        }

        let timestamp = Duration::from_secs_f64(
            self.samples_read as f64 / self.sample_rate as f64 / self.channels as f64,
        );
        self.samples_read += samples.len() as u64;

        Some(AudioChunk::new(
            samples,
            timestamp,
            self.sample_rate,
            self.channels,
        ))
    }

    /// Returns the number of samples currently in the buffer.
    pub fn available(&self) -> usize {
        self.consumer.occupied_len()
    }

    /// Returns true if enough samples are available for a complete chunk.
    pub fn has_chunk(&self) -> bool {
        self.available() >= self.chunk_size
    }

    /// Drains all remaining samples from the buffer.
    ///
    /// Returns chunks until the buffer is empty. The last chunk may be
    /// smaller than the configured chunk size.
    pub fn drain(&mut self) -> Vec<AudioChunk> {
        let mut chunks = Vec::new();

        // First, read all complete chunks
        while self.has_chunk() {
            if let Some(chunk) = self.try_read_chunk() {
                chunks.push(chunk);
            }
        }

        // Then read any remaining samples as a final partial chunk
        let remaining = self.available();
        if remaining > 0 {
            let mut samples = Vec::with_capacity(remaining);
            while let Some(sample) = self.consumer.try_pop() {
                samples.push(sample);
            }

            if !samples.is_empty() {
                let timestamp = Duration::from_secs_f64(
                    self.samples_read as f64 / self.sample_rate as f64 / self.channels as f64,
                );
                self.samples_read += samples.len() as u64;

                chunks.push(AudioChunk::new(
                    samples,
                    timestamp,
                    self.sample_rate,
                    self.channels,
                ));
            }
        }

        chunks
    }
}

/// Creates a ring buffer pair for audio capture.
///
/// Returns a producer (for the CPAL callback) and an `AudioBuffer` (for the router).
#[allow(dead_code)] // Used in tests and may be useful for custom setups
pub fn create_audio_buffer(
    capacity_duration: Duration,
    sample_rate: u32,
    channels: u16,
    chunk_duration: Duration,
) -> (ringbuf::HeapProd<i16>, AudioBuffer) {
    let capacity =
        (sample_rate as f64 * capacity_duration.as_secs_f64()) as usize * channels as usize;

    let ring_buffer = HeapRb::<i16>::new(capacity);
    let (producer, consumer) = ring_buffer.split();

    let buffer = AudioBuffer::new(consumer, sample_rate, channels, chunk_duration);

    (producer, buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Producer;

    #[test]
    fn test_audio_buffer_read_chunk() {
        let (mut producer, mut buffer) =
            create_audio_buffer(Duration::from_secs(1), 16000, 1, Duration::from_millis(100));

        // Push 100ms worth of samples (1600 at 16kHz)
        for i in 0..1600i16 {
            let _ = producer.try_push(i);
        }

        assert!(buffer.has_chunk());
        let chunk = buffer.try_read_chunk().unwrap();
        assert_eq!(chunk.samples.len(), 1600);
        assert_eq!(chunk.sample_rate, 16000);
        assert_eq!(chunk.channels, 1);
    }

    #[test]
    fn test_audio_buffer_not_enough_samples() {
        let (mut producer, mut buffer) =
            create_audio_buffer(Duration::from_secs(1), 16000, 1, Duration::from_millis(100));

        // Push only 50ms worth
        for i in 0..800i16 {
            let _ = producer.try_push(i);
        }

        assert!(!buffer.has_chunk());
        assert!(buffer.try_read_chunk().is_none());
    }

    #[test]
    fn test_audio_buffer_drain() {
        let (mut producer, mut buffer) =
            create_audio_buffer(Duration::from_secs(1), 16000, 1, Duration::from_millis(100));

        // Push 250ms worth (2.5 chunks)
        for i in 0..4000i16 {
            let _ = producer.try_push(i % 1000);
        }

        let chunks = buffer.drain();
        assert_eq!(chunks.len(), 3); // 2 full chunks + 1 partial
        assert_eq!(chunks[0].samples.len(), 1600);
        assert_eq!(chunks[1].samples.len(), 1600);
        assert_eq!(chunks[2].samples.len(), 800); // remaining
    }

    #[test]
    fn test_audio_buffer_timestamp() {
        let (mut producer, mut buffer) =
            create_audio_buffer(Duration::from_secs(1), 16000, 1, Duration::from_millis(100));

        // Push 200ms worth
        for _ in 0..3200i16 {
            let _ = producer.try_push(0);
        }

        let chunk1 = buffer.try_read_chunk().unwrap();
        let chunk2 = buffer.try_read_chunk().unwrap();

        assert_eq!(chunk1.timestamp, Duration::from_millis(0));
        assert_eq!(chunk2.timestamp, Duration::from_millis(100));
    }
}
