# stream-audio

> **Note:** This crate is under active development. The API may change before 1.0.

Real-time audio capture with multi-sink architecture.

[![CI](https://github.com/lotl-co/stream-audio/actions/workflows/ci.yml/badge.svg)](https://github.com/lotl-co/stream-audio/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/stream-audio.svg)](https://crates.io/crates/stream-audio)
[![Documentation](https://docs.rs/stream-audio/badge.svg)](https://docs.rs/stream-audio)
[![license](https://img.shields.io/crates/l/stream-audio)](LICENSE)

## Features

- Non-blocking audio capture via CPAL
- Multiple simultaneous destinations (file, channel, custom)
- Resilient buffering - never drops audio due to slow consumers
- Simple builder API with sensible defaults
- Fully async with Tokio

## Scope

This crate is designed for **local/desktop audio capture** - capturing audio from system devices (microphones, system audio, loopback) on end-user machines.

**Out of scope:** Server-side audio ingestion (WebRTC, SIP/telephony streams, RTP). These use cases have fundamentally different requirements (async network I/O vs. real-time device callbacks) and are better served by a separate implementation that can share downstream processing components.

## Quick Start

```rust
use stream_audio::{StreamAudio, AudioSource, FileSink, ChannelSink, FormatPreset};
use tokio::sync::mpsc;

let (tx, rx) = mpsc::channel::<AudioChunk>(32);

let session = StreamAudio::builder()
    .add_source("mic", AudioSource::default_device())
    .format(FormatPreset::Transcription)           // 16kHz mono
    .add_sink(FileSink::wav("meeting.wav"))
    .add_sink(ChannelSink::new(tx))
    .on_event(|e| tracing::warn!(?e, "stream event"))
    .start()
    .await?;

// Process chunks as they arrive
while let Some(chunk) = rx.recv().await {
    // Send to Deepgram, Whisper, etc.
}

session.stop().await?;
```

## Multi-Source Capture

Capture from multiple audio sources simultaneously with routing to different sinks:

```rust
let session = StreamAudio::builder()
    .add_source("mic", AudioSource::device("MacBook Pro Microphone"))
    .add_source("speaker", AudioSource::device("BlackHole 2ch"))
    // Route to specific sources
    .add_sink_from(FileSink::wav("mic.wav"), "mic")
    .add_sink_from(FileSink::wav("speaker.wav"), "speaker")
    // Merge multiple sources into one sink
    .add_sink_merged(FileSink::wav("merged.wav"), ["mic", "speaker"])
    .format(FormatPreset::Transcription)
    .start()
    .await?;
```

## Glossary

*Consistent terminology helps everyone speak the same language.*

| Term | Description |
|------|-------------|
| **Source** | Where audio originates (microphone, system audio, file) |
| **Sink** | Where audio flows to (file, channel, network) |
| **Chunk** | A buffer of audio samples with timing metadata |
| **Session** | An active capture session with start/stop lifecycle |

## Architecture

```
CPAL AUDIO THREAD (high-priority, never blocks)
  └── CPAL Callback → Ring Buffer (30s default)
                          │
                          ▼ (async recv)
TOKIO RUNTIME
  └── Router Task → FileSink, ChannelSink, CustomSink...
```

Internally, the crate separates real-time capture (CPAL callback thread) from processing (Tokio tasks). The callback
pushes samples into a lock-free ring buffer; Tokio drains the buffer and delivers audio to one or more sinks. This
guarantees that audio capture never blocks even under backpressure.

**Key invariant:** The CPAL callback never waits. If sinks are slow, the ring buffer absorbs pressure.

## Limitations

**WAV file size:** WAV files are limited to ~4GB due to 32-bit size headers. At 16kHz mono (default), this allows ~37 hours of recording. For longer sessions, consider splitting files or using a streaming sink.

**Resampling:** When the device sample rate differs from the target format, linear interpolation is used for resampling. This is optimized for low-latency real-time capture rather than maximum audio fidelity. For high-quality offline resampling, process the raw device output externally.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
