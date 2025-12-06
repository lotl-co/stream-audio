//! Integration tests for stream-audio.
//!
//! Note: Tests that require actual audio hardware are marked with
//! `#[ignore]` and should be run manually.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use stream_audio::{AudioChunk, ChannelSink, FileSink, Sink, SinkError};
use tokio::sync::mpsc;

/// A test sink that counts writes.
struct CountingSink {
    name: String,
    count: AtomicUsize,
}

impl CountingSink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            count: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Sink for CountingSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn write(&self, _chunk: &AudioChunk) -> Result<(), SinkError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn test_channel_sink_receives_data() {
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
    let sink = ChannelSink::new(tx);

    // Write a chunk
    let chunk = AudioChunk::new(vec![1, 2, 3, 4, 5], Duration::ZERO, 16000, 1);
    sink.write(&chunk).await.unwrap();

    // Receive it
    let received = rx.recv().await.unwrap();
    assert_eq!(received.samples, vec![1, 2, 3, 4, 5]);
    assert_eq!(received.sample_rate, 16000);
    assert_eq!(received.channels, 1);
}

#[tokio::test]
async fn test_file_sink_round_trip() {
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = dir.path().join("test_output.wav");

    let sink = FileSink::wav(&path);

    // Start sink
    sink.on_start().await.unwrap();

    // Write some audio (1 second at 16kHz mono)
    let samples: Vec<i16> = (0..16000)
        .map(|i| ((i as f32 / 16.0).sin() * 10000.0) as i16)
        .collect();
    let chunk = AudioChunk::new(samples.clone(), Duration::ZERO, 16000, 1);
    sink.write(&chunk).await.unwrap();

    // Stop sink (finalizes WAV header)
    sink.on_stop().await.unwrap();

    // Verify file exists and has correct size
    let metadata = std::fs::metadata(&path).unwrap();
    // WAV header (44 bytes) + 16000 samples * 2 bytes = 32044 bytes
    assert_eq!(metadata.len(), 44 + 16000 * 2);

    // Verify WAV header
    let data = std::fs::read(&path).unwrap();
    assert_eq!(&data[0..4], b"RIFF");
    assert_eq!(&data[8..12], b"WAVE");
}

#[tokio::test]
async fn test_multiple_sinks() {
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
    let channel_sink = Arc::new(ChannelSink::new(tx));
    let counting_sink = Arc::new(CountingSink::new("counter"));

    // Simulate writing to multiple sinks
    let chunk = AudioChunk::new(vec![100, 200, 300], Duration::ZERO, 16000, 1);

    channel_sink.write(&chunk).await.unwrap();
    counting_sink.write(&chunk).await.unwrap();

    // Verify both received
    let received = rx.recv().await.unwrap();
    assert_eq!(received.samples, vec![100, 200, 300]);
    assert_eq!(counting_sink.count(), 1);
}

#[tokio::test]
async fn test_audio_chunk_duration() {
    // 100ms at 16kHz mono
    let chunk = AudioChunk::new(vec![0; 1600], Duration::ZERO, 16000, 1);
    assert_eq!(chunk.duration(), Duration::from_millis(100));

    // 100ms at 48kHz stereo
    let chunk = AudioChunk::new(vec![0; 9600], Duration::ZERO, 48000, 2);
    assert_eq!(chunk.duration(), Duration::from_millis(100));
}

#[tokio::test]
async fn test_mock_source_generates_audio() {
    use stream_audio::MockSource;

    let mut mock = MockSource::transcription();

    // Generate 500ms of audio
    mock.generate_silence(250);
    mock.generate_sine(440.0, 250);

    assert_eq!(mock.duration(), Duration::from_millis(500));

    let samples = mock.take_samples();
    // 500ms at 16kHz = 8000 samples
    assert_eq!(samples.len(), 8000);
}

#[tokio::test]
async fn test_format_conversion() {
    use stream_audio::format::{f32_to_i16, stereo_to_mono};

    // Test f32 to i16
    assert_eq!(f32_to_i16(1.0), 32767);
    assert_eq!(f32_to_i16(-1.0), -32767);
    assert_eq!(f32_to_i16(0.0), 0);

    // Test stereo to mono
    let stereo = vec![1000i16, 2000, 3000, 4000];
    let mono = stereo_to_mono(&stereo);
    assert_eq!(mono, vec![1500, 3500]);
}

#[tokio::test]
async fn test_resampling() {
    use stream_audio::format::resample;

    // Downsample 48kHz to 16kHz
    let samples: Vec<i16> = (0..4800).map(|i| i as i16).collect();
    let resampled = resample(&samples, 48000, 16000);

    // Should be ~1/3 the size
    assert_eq!(resampled.len(), 1600);
}

/// Tests the full pipeline using MockSource without requiring hardware.
///
/// This manually wires up the components that StreamAudio::builder() would
/// create, but uses MockSource instead of a real audio device.
#[tokio::test]
async fn test_full_pipeline_with_mock_source() {
    use stream_audio::MockSource;

    // Create mock audio source with test data
    let mut mock = MockSource::transcription(); // 16kHz mono
    mock.generate_sine(440.0, 200); // 200ms of 440Hz tone

    // Get the ring buffer consumer (simulates what AudioDevice::start_capture returns)
    let ring_consumer = mock.into_ring_buffer();

    // Create a channel sink to receive the output
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);
    let sink = Arc::new(ChannelSink::new(tx));

    // Manually create and run the router (simulates what builder does)
    use stream_audio::format::FormatConverter;

    let converter = FormatConverter::new(16000, 1, 16000, 1); // passthrough
    let samples: Vec<i16> = {
        let mut consumer = ring_consumer;
        let mut samples = Vec::new();
        use ringbuf::traits::Consumer;
        while let Some(sample) = consumer.try_pop() {
            samples.push(sample);
        }
        samples
    };

    // Convert and send through sink
    let converted = converter.convert(&samples);
    let chunk = AudioChunk::new(converted, Duration::ZERO, 16000, 1);
    sink.write(&chunk).await.unwrap();

    // Verify we received the audio
    let received = rx.recv().await.unwrap();
    assert_eq!(received.sample_rate, 16000);
    assert_eq!(received.channels, 1);
    // 200ms at 16kHz = 3200 samples
    assert_eq!(received.samples.len(), 3200);
    // Verify it's not silence (sine wave should have non-zero samples)
    assert!(received.samples.iter().any(|&s| s != 0));
}

/// This test requires actual audio hardware and should be run manually.
#[tokio::test]
#[ignore = "requires audio hardware"]
async fn test_real_capture() {
    use stream_audio::{FormatPreset, StreamAudio};

    let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);

    let session = StreamAudio::builder()
        .format(FormatPreset::Transcription)
        .add_sink(ChannelSink::new(tx))
        .start()
        .await
        .expect("Failed to start capture");

    // Capture for 1 second
    let timeout = tokio::time::timeout(Duration::from_secs(1), async {
        let mut total_samples = 0;
        while let Some(chunk) = rx.recv().await {
            total_samples += chunk.samples.len();
            if total_samples > 16000 {
                break;
            }
        }
        total_samples
    })
    .await;

    session.stop().await.expect("Failed to stop capture");

    if let Ok(samples) = timeout {
        println!("Captured {} samples", samples);
        assert!(samples > 0, "Should have captured some audio");
    }
}
