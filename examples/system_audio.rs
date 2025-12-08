//! System audio capture example.
//!
//! Demonstrates capturing system audio (what you hear through speakers)
//! along with microphone input, without requiring virtual audio devices.
//!
//! Run with: cargo run --example system_audio --features system-audio
//!
//! Requirements:
//! - macOS 14.2+ (Sonoma or later) - uses Core Audio Taps
//! - Screen Recording permission (prompted on first run)

#[cfg(not(feature = "system-audio"))]
fn main() {
    eprintln!("This example requires the 'system-audio' feature.");
    eprintln!("Run with: cargo run --example system_audio --features system-audio");
}

#[cfg(feature = "system-audio")]
use std::time::Duration;
#[cfg(feature = "system-audio")]
use stream_audio::{AudioChunk, AudioSource, ChannelSink, FileSink, FormatPreset, StreamAudio};
#[cfg(feature = "system-audio")]
use tokio::sync::mpsc;

#[cfg(feature = "system-audio")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== System Audio Capture Example ===\n");
    println!("This example captures:");
    println!("  - Your microphone (what you say)");
    println!("  - System audio (what you hear - video calls, music, etc.)\n");

    println!("Note: On first run, macOS may prompt for Screen Recording permission.");
    println!("Grant permission in System Preferences > Privacy & Security > Screen Recording.");
    println!("Requires macOS 14.2+ (Sonoma) for Core Audio Taps support.\n");

    // Create a channel to receive merged audio chunks
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);

    println!("Starting capture...");
    println!("Recording for 10 seconds.\n");

    // Build session with mic + system audio
    let session = StreamAudio::builder()
        // Microphone input
        .add_source("mic", AudioSource::default_device())
        // System audio - captures speaker output without BlackHole/Soundflower
        .add_source("speaker", AudioSource::system_audio())
        // Save individual tracks
        .add_sink_from(FileSink::wav("mic_only.wav"), "mic")
        .add_sink_from(FileSink::wav("speaker_only.wav"), "speaker")
        // Save merged audio (both tracks combined)
        .add_sink_merged(FileSink::wav("meeting.wav"), ["mic", "speaker"])
        // Send merged audio to channel for real-time processing
        .add_sink_merged(ChannelSink::new(tx), ["mic", "speaker"])
        // Use transcription-optimized format
        .format(FormatPreset::Transcription)
        .on_event(|event| {
            println!("Event: {event:?}");
        })
        .start()
        .await?;

    // Count received chunks in background
    let chunk_counter = tokio::spawn(async move {
        let mut count = 0usize;
        let mut total_samples = 0usize;
        while let Some(chunk) = rx.recv().await {
            count += 1;
            total_samples += chunk.samples.len();
            // Print progress every ~1 second
            if count % 10 == 0 {
                let duration_ms = (total_samples as f64 / 16.0) as u64; // 16kHz
                println!("  Captured {duration_ms}ms of audio ({count} chunks)");
            }
        }
        (count, total_samples)
    });

    // Record for 10 seconds
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Stop and get stats
    let stats = session.stats();
    session.stop().await?;

    let (chunk_count, sample_count) = chunk_counter.await?;

    println!("\n=== Recording Complete ===\n");
    println!("Statistics:");
    println!("  Total chunks processed: {}", stats.chunks_processed);
    println!("  Total samples captured: {}", stats.samples_captured);
    println!("  Buffer overflows: {}", stats.buffer_overflows);
    println!("  Merged chunks received: {chunk_count}");
    println!("  Merged samples: {sample_count}");

    println!("\nFiles saved:");
    println!("  - mic_only.wav     (microphone audio)");
    println!("  - speaker_only.wav (system audio)");
    println!("  - meeting.wav      (merged - both tracks)");

    println!("\nTip: The merged file contains both your voice and what others said,");
    println!("perfect for meeting transcription!");

    Ok(())
}
