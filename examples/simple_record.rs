//! Simple recording example.
//!
//! Records audio from the default input device to a WAV file.
//!
//! Run with: cargo run --example simple_record

use std::time::Duration;
use stream_audio::{FileSink, FormatPreset, StreamAudio};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing for debug output
    tracing_subscriber::fmt::init();

    println!("Recording to recording.wav for 5 seconds...");
    println!("Press Ctrl+C to stop early.");

    // Start recording with minimal configuration
    let session = StreamAudio::builder()
        .format(FormatPreset::Transcription) // 16kHz mono
        .add_sink(FileSink::wav("recording.wav"))
        .start()
        .await?;

    // Record for 5 seconds
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Get stats before stopping (stop() consumes the session)
    let stats = session.stats();

    // Stop and finalize the WAV file
    session.stop().await?;

    println!("Recording saved to recording.wav");
    println!("Stats: {:?}", stats);

    Ok(())
}
