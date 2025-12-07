//! Multi-source/multi-sink capture example.
//!
//! Demonstrates capturing from multiple audio sources simultaneously
//! with routing to different sinks.
//!
//! Run with: cargo run --example multi
//!
//! Note: This example requires two audio input devices to be available.

use std::io::{self, Write};
use std::time::Duration;
use stream_audio::{
    AudioChunk, AudioSource, ChannelSink, FileSink, FormatPreset, StreamAudio, StreamEvent,
};
use tokio::sync::mpsc;

/// Prompts user to select a device from the list.
fn select_device(devices: &[String], prompt: &str) -> Option<String> {
    println!("\n{prompt}");
    println!("  0. System default");
    for (i, device) in devices.iter().enumerate() {
        println!("  {}. {}", i + 1, device);
    }

    print!("Enter selection: ");
    io::stdout().flush().ok()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok()?;

    let selection: usize = input.trim().parse().ok()?;

    if selection == 0 {
        None // Use default
    } else {
        devices.get(selection - 1).cloned()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // List available devices
    let devices = stream_audio::list_input_devices().unwrap_or_default();

    println!("=== Multi-Source Audio Capture ===\n");
    println!("Available input devices:");
    for device in &devices {
        println!("  - {device}");
    }

    if devices.is_empty() {
        eprintln!("No input devices found!");
        return Ok(());
    }

    // Prompt user to select devices
    let mic_device = select_device(&devices, "Select MICROPHONE source:");
    let speaker_device = select_device(&devices, "Select SPEAKER/SYSTEM source:");

    // Build sources based on selection
    let mic_source = match &mic_device {
        Some(name) => AudioSource::device(name),
        None => AudioSource::default_device(),
    };

    let speaker_source = match &speaker_device {
        Some(name) => AudioSource::device(name),
        None => AudioSource::default_device(),
    };

    println!("\nConfiguration:");
    println!(
        "  Mic: {}",
        mic_device.as_deref().unwrap_or("(system default)")
    );
    println!(
        "  Speaker: {}",
        speaker_device.as_deref().unwrap_or("(system default)")
    );

    // Create channels for real-time processing
    let (mic_tx, mut mic_rx) = mpsc::channel::<AudioChunk>(100);
    let (speaker_tx, mut speaker_rx) = mpsc::channel::<AudioChunk>(100);

    println!("\nStarting multi-source capture...");
    println!("Recording for 5 seconds with:");
    println!("  - Microphone -> mic.wav + transcription channel");
    println!("  - Speaker -> speaker.wav + transcription channel");
    println!("  - Merged -> merged.wav");
    println!();

    // Start multi-source capture
    let session = StreamAudio::builder()
        // Add sources with IDs
        .add_source("mic", mic_source)
        .add_source("speaker", speaker_source)
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
