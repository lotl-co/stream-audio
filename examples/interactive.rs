//! Interactive multi-source audio capture example.
//!
//! Demonstrates selecting multiple audio sources interactively and recording
//! until Ctrl-C is pressed.
//!
//! # Usage
//!
//! ```bash
//! # With system audio support (macOS):
//! cargo run --example interactive --features sck-native
//!
//! # Without system audio:
//! cargo run --example interactive
//! ```

use std::io::{self, Write};
use stream_audio::{AudioSource, FileSink, FormatPreset, StreamAudio};

/// Represents a selectable audio source
struct SourceOption {
    name: String,
    source: AudioSource,
    is_system_audio: bool,
}

fn list_available_sources() -> Vec<SourceOption> {
    let mut sources = Vec::new();

    if let Ok(devices) = stream_audio::list_input_devices() {
        for device in devices {
            sources.push(SourceOption {
                name: device.clone(),
                source: AudioSource::device(&device),
                is_system_audio: false,
            });
        }
    }

    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    {
        sources.push(SourceOption {
            name: "System Audio (macOS)".to_string(),
            source: AudioSource::system_audio(),
            is_system_audio: true,
        });
    }

    sources
}

fn prompt_source_selection(sources: &[SourceOption]) -> Vec<usize> {
    println!("\nAvailable audio sources:");
    println!("------------------------");
    for (i, source) in sources.iter().enumerate() {
        let label = if source.is_system_audio {
            "[System]"
        } else {
            "[Mic]"
        };
        println!("  {}. {} {}", i + 1, label, source.name);
    }

    println!();
    println!("Enter source numbers separated by commas (e.g., 1,3)");
    println!("Or press Enter for default microphone only");
    print!("> ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return vec![0]; // Default to first source
    }

    let input = input.trim();
    if input.is_empty() {
        return vec![0]; // Default to first source
    }

    input
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|&n| n > 0 && n <= sources.len())
        .map(|n| n - 1) // Convert to 0-indexed
        .collect()
}

fn prompt_output_filename() -> String {
    print!("\nOutput filename [recording.wav]: ");
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() || input.trim().is_empty() {
        return "recording.wav".to_string();
    }

    let filename = input.trim().to_string();
    if filename.ends_with(".wav") {
        filename
    } else {
        format!("{filename}.wav")
    }
}

fn generate_source_id(source: &SourceOption, index: usize) -> String {
    if source.is_system_audio {
        "system".to_string()
    } else {
        format!("mic_{}", index)
    }
}

/// Sanitizes a device name for use as a filename.
/// "MacBook Pro Microphone" → "macbook_pro_microphone"
fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    // Remove leading/trailing underscores and collapse multiple underscores
    sanitized
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== Interactive Audio Capture ===");
    println!();

    let sources = list_available_sources();
    if sources.is_empty() {
        eprintln!("No audio sources found!");
        return Ok(());
    }

    let selected_indices = prompt_source_selection(&sources);
    if selected_indices.is_empty() {
        eprintln!("No sources selected!");
        return Ok(());
    }

    let output_file = prompt_output_filename();

    let mut builder = StreamAudio::builder().format(FormatPreset::Transcription);

    let mut source_ids = Vec::new();
    println!("\nSelected sources:");
    for (i, &idx) in selected_indices.iter().enumerate() {
        let source = &sources[idx];
        let source_id = generate_source_id(source, i);
        println!("  - {} (id: {})", source.name, source_id);

        builder = builder.add_source(source_id.as_str(), source.source.clone());
        source_ids.push(source_id);
    }

    println!("\nOutput files:");
    for (idx, source_id) in source_ids.iter().enumerate() {
        let source = &sources[selected_indices[idx]];
        let filename = if source_ids.len() == 1 {
            output_file.clone()
        } else {
            format!("{}.wav", sanitize_filename(&source.name))
        };
        builder = builder.add_sink_from(FileSink::wav(&filename), source_id.as_str());
        println!("  - {} (from {})", filename, source.name);
    }

    builder = builder.on_event(|event| match event {
        stream_audio::StreamEvent::SourceStarted { source_id } => {
            println!("[Event] Source started: {}", source_id);
        }
        stream_audio::StreamEvent::SourceStopped { source_id, reason } => {
            println!("[Event] Source stopped: {} - {}", source_id, reason);
        }
        stream_audio::StreamEvent::BufferOverflow { dropped_ms } => {
            eprintln!("[Warning] Buffer overflow: dropped {}ms", dropped_ms);
        }
        _ => {}
    });

    println!();
    println!("Starting capture... Press Ctrl-C to stop.");
    println!();

    let session = builder.start().await?;

    tokio::signal::ctrl_c().await?;

    println!();
    println!("Stopping capture...");

    let stats = session.stats();
    session.stop().await?;

    println!();
    println!("Recording complete!");
    println!("Stats:");
    println!("  Chunks processed: {}", stats.chunks_processed);
    println!("  Samples captured: {}", stats.samples_captured);
    println!("  Buffer overflows: {}", stats.buffer_overflows);

    Ok(())
}
