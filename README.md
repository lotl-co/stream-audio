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

## Quick Start

```rust
use stream_audio::{StreamAudio, FileSink, ChannelSink, FormatPreset};
use tokio::sync::mpsc;

let (tx, rx) = mpsc::channel::<AudioChunk>(100);

let session = StreamAudio::builder()
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

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
