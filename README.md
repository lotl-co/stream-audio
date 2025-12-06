# stream-audio

> **Note:** This crate is a work in progress. It currently does nothing functional and exists to reserve the crate name. Check back soon!

Real-time audio capture with multi-sink architecture.

[![CI](https://github.com/lotl-co/stream-audio/actions/workflows/ci.yml/badge.svg)](https://github.com/lotl-co/stream-audio/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/stream-audio.svg)](https://crates.io/crates/stream-audio)
[![Documentation](https://docs.rs/stream-audio/badge.svg)](https://docs.rs/stream-audio)
[![License](https://img.shields.io/crates/l/stream-audio.svg)](LICENSE)

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

**Key invariant:** The CPAL callback never waits. If sinks are slow, the ring buffer absorbs pressure.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.
