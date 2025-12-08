//! ScreenCaptureKit audio capture example.
//!
//! Demonstrates capturing system audio via ScreenCaptureKit, which allows
//! per-app audio capture on macOS 12.3+.
//!
//! Run with: cargo run --example screencapturekit --features screencapturekit
//!
//! Requirements:
//! - macOS 12.3+ (Monterey or later)
//! - Screen Recording permission (prompted on first run)

#[cfg(not(feature = "screencapturekit"))]
fn main() {
    eprintln!("This example requires the 'screencapturekit' feature.");
    eprintln!("Run with: cargo run --example screencapturekit --features screencapturekit");
}

#[cfg(feature = "screencapturekit")]
use std::time::Duration;
#[cfg(feature = "screencapturekit")]
use stream_audio::{
    AudioChunk, AudioSource, ChannelSink, FileSink, FormatPreset, ScreenCaptureKitConfig,
    StreamAudio,
};
#[cfg(feature = "screencapturekit")]
use tokio::sync::mpsc;

#[cfg(feature = "screencapturekit")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== ScreenCaptureKit Audio Capture Example ===\n");
    println!("This example captures system audio using ScreenCaptureKit.");
    println!("Unlike Core Audio Taps, SCK can capture audio from specific apps.\n");

    println!("Note: On first run, macOS will prompt for Screen Recording permission.");
    println!("Grant permission in System Preferences > Privacy & Security > Screen Recording.\n");

    // Create a channel to receive audio chunks
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);

    println!("Starting capture of all system audio...");
    println!("Recording for 10 seconds.\n");

    // Capture all apps using ScreenCaptureKit
    let session = StreamAudio::builder()
        .add_source(
            "speaker",
            AudioSource::screencapturekit(ScreenCaptureKitConfig::all_apps()),
        )
        .add_sink(FileSink::wav("sck_capture.wav"))
        .add_sink(ChannelSink::new(tx))
        .format(FormatPreset::Transcription) // 16kHz mono
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
    println!("  Chunks received: {chunk_count}");
    println!("  Samples received: {sample_count}");

    println!("\nFile saved: sck_capture.wav");
    println!("\n--- Per-App Capture Example ---\n");
    println!("To capture audio from a specific app, use:");
    println!("  AudioSource::screencapturekit(");
    println!("      ScreenCaptureKitConfig::app_by_bundle_id(\"us.zoom.xos\")");
    println!("  )");
    println!("\nCommon bundle IDs:");
    println!("  - us.zoom.xos (Zoom)");
    println!("  - com.spotify.client (Spotify)");
    println!("  - com.apple.Safari (Safari)");
    println!("  - com.google.Chrome (Chrome)");

    Ok(())
}
