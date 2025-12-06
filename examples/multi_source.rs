//! Multi-source capture example.
//!
//! Demonstrates capturing from multiple audio sources simultaneously
//! with routing to different sinks.
//!
//! Run with: cargo run --example multi_source
//!
//! Note: This example requires two audio input devices to be available.
//! If you only have one device, modify the example to use MockSource.

use std::time::Duration;
use stream_audio::{
    AudioChunk, AudioSource, ChannelSink, FileSink, FormatPreset, StreamAudio, StreamEvent,
};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // List available devices
    println!("Available input devices:");
    if let Ok(devices) = stream_audio::list_input_devices() {
        for device in devices {
            println!("  - {device}");
        }
    }
    println!();

    // Create channels for real-time processing
    let (mic_tx, mut mic_rx) = mpsc::channel::<AudioChunk>(100);
    let (speaker_tx, mut speaker_rx) = mpsc::channel::<AudioChunk>(100);

    // For demonstration, we'll use the default device for both sources.
    // In a real application, you'd use different devices:
    //   .add_source("mic", AudioSource::device("MacBook Pro Microphone"))
    //   .add_source("speaker", AudioSource::device("BlackHole 2ch"))

    println!("Starting multi-source capture...");
    println!("Recording for 5 seconds with:");
    println!("  - Microphone → mic.wav + transcription channel");
    println!("  - Speaker → speaker.wav + transcription channel");
    println!("  - Merged → merged.wav");
    println!();

    // Start multi-source capture
    let session = StreamAudio::builder()
        // Add sources with IDs
        .add_source("mic", AudioSource::default_device())
        .add_source("speaker", AudioSource::default_device())
        // Route sinks to specific sources
        .add_sink_from(FileSink::wav("mic.wav"), "mic")
        .add_sink_from(FileSink::wav("speaker.wav"), "speaker")
        .add_sink_merged(FileSink::wav("merged.wav"), ["mic", "speaker"])
        // Add channel sinks for real-time processing
        .add_sink_from(ChannelSink::new(mic_tx), "mic")
        .add_sink_from(ChannelSink::new(speaker_tx), "speaker")
        .format(FormatPreset::Transcription)
        .on_event(|event| match event {
            StreamEvent::BufferOverflow { dropped_ms } => {
                eprintln!("Warning: dropped {dropped_ms}ms of audio");
            }
            StreamEvent::SinkError { sink_name, error } => {
                eprintln!("Sink '{sink_name}' error: {error}");
            }
            StreamEvent::SourceStarted { source_id } => {
                println!("Source started: {source_id}");
            }
            StreamEvent::SourceStopped { source_id, reason } => {
                println!("Source stopped: {source_id} - {reason}");
            }
            StreamEvent::MergeIncomplete { window_id, missing } => {
                eprintln!("Merge window {window_id} incomplete, missing: {missing:?}");
            }
            _ => {}
        })
        .start()
        .await?;

    // Spawn tasks to count received chunks
    let mic_counter = tokio::spawn(async move {
        let mut count = 0;
        while let Some(_chunk) = mic_rx.recv().await {
            count += 1;
        }
        count
    });

    let speaker_counter = tokio::spawn(async move {
        let mut count = 0;
        while let Some(_chunk) = speaker_rx.recv().await {
            count += 1;
        }
        count
    });

    // Record for 5 seconds
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Stop recording
    let stats = session.stats();
    session.stop().await?;

    // Get final counts
    let mic_chunks = mic_counter.await?;
    let speaker_chunks = speaker_counter.await?;

    println!("\nRecording complete!");
    println!("Stats:");
    println!("  Chunks processed: {}", stats.chunks_processed);
    println!("  Samples captured: {}", stats.samples_captured);
    println!("  Buffer overflows: {}", stats.buffer_overflows);
    println!("  Mic channel chunks: {mic_chunks}");
    println!("  Speaker channel chunks: {speaker_chunks}");
    println!("\nFiles saved:");
    println!("  - mic.wav (microphone only)");
    println!("  - speaker.wav (speaker only)");
    println!("  - merged.wav (combined audio)");

    Ok(())
}
