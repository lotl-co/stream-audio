//! System audio capture example via native ScreenCaptureKit backend.
//!
//! Captures system audio for a few seconds and saves to a WAV file.
//!
//! # Requirements
//!
//! - macOS 13.0 or later
//! - Screen Recording permission granted
//! - The `sck-native` feature enabled
//!
//! # Usage
//!
//! ```bash
//! cargo run --example system_audio --features sck-native
//! ```

#[cfg(not(all(target_os = "macos", feature = "sck-native")))]
fn main() {
    eprintln!("This example requires macOS and the 'sck-native' feature.");
    eprintln!("Run with: cargo run --example system_audio --features sck-native");
}

#[cfg(all(target_os = "macos", feature = "sck-native"))]
use std::time::Duration;

#[cfg(all(target_os = "macos", feature = "sck-native"))]
use stream_audio::{AudioSource, FileSink, FormatPreset, StreamAudio};

#[cfg(all(target_os = "macos", feature = "sck-native"))]
use tracing_subscriber::EnvFilter;

#[cfg(all(target_os = "macos", feature = "sck-native"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Set up logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("System Audio Capture Example");
    println!("============================");
    println!();
    println!("This will capture system audio for 5 seconds.");
    println!("Make sure you have Screen Recording permission granted.");
    println!();
    println!("Play some audio (music, video, etc.) to test capture.");
    println!();

    // Create session with system audio source
    let session = StreamAudio::builder()
        .add_source("speaker", AudioSource::system_audio())
        .format(FormatPreset::Transcription) // 16kHz mono
        .add_sink(FileSink::wav("system_audio_capture.wav"))
        .on_event(|event| {
            println!("Event: {event:?}");
        })
        .start()
        .await?;

    println!("Recording for 5 seconds...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get stats before stopping
    let stats = session.stats();
    println!();
    println!("Stats:");
    println!("  Chunks processed: {}", stats.chunks_processed);
    println!("  Samples captured: {}", stats.samples_captured);
    println!("  Buffer overflows: {}", stats.buffer_overflows);

    // Stop and save
    session.stop().await?;

    println!();
    println!("Saved to: system_audio_capture.wav");
    println!("Done!");

    Ok(())
}
