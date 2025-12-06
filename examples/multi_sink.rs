//! Multi-sink example.
//!
//! Demonstrates recording to both a file and a channel simultaneously.
//! The channel can be used for real-time processing (e.g., transcription).
//!
//! Run with: cargo run --example multi_sink

use std::time::Duration;
use stream_audio::{AudioChunk, ChannelSink, FileSink, FormatPreset, StreamAudio, StreamEvent};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Create a channel to receive audio chunks
    let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);

    println!("Recording to multi_sink.wav and processing in real-time...");
    println!("Recording for 5 seconds.");

    // Start recording with multiple sinks
    let session = StreamAudio::builder()
        .format(FormatPreset::Transcription)
        .add_sink(FileSink::wav("multi_sink.wav"))
        .add_sink(ChannelSink::new(tx))
        .on_event(|event| match event {
            StreamEvent::BufferOverflow { dropped_ms } => {
                eprintln!("Warning: dropped {}ms of audio", dropped_ms);
            }
            StreamEvent::SinkError { sink_name, error } => {
                eprintln!("Sink '{}' error: {}", sink_name, error);
            }
            StreamEvent::StreamInterrupted { reason } => {
                eprintln!("Stream interrupted: {}", reason);
            }
        })
        .start()
        .await?;

    // Spawn a task to process received chunks
    let processor = tokio::spawn(async move {
        let mut total_samples = 0;
        let mut chunk_count = 0;

        while let Some(chunk) = rx.recv().await {
            total_samples += chunk.samples.len();
            chunk_count += 1;

            // In a real application, you might send this to a transcription service
            if chunk_count % 10 == 0 {
                let duration_ms = total_samples as f64 / 16.0; // 16kHz = 16 samples/ms
                println!(
                    "Processed {} chunks ({:.0}ms of audio)",
                    chunk_count, duration_ms
                );
            }
        }

        (chunk_count, total_samples)
    });

    // Record for 5 seconds
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Stop recording
    session.stop().await?;

    // Wait for processor to finish
    let (chunks, samples) = processor.await?;
    let duration_secs = samples as f64 / 16000.0;

    println!("\nRecording complete!");
    println!(
        "Processed {} chunks, {} samples ({:.2}s)",
        chunks, samples, duration_secs
    );
    println!("File saved to multi_sink.wav");

    Ok(())
}
