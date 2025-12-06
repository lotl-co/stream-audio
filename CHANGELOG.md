# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2024-12-05

Initial release of stream-audio.

### Added

#### Core API
- `StreamAudio::builder()` - fluent API for configuring audio capture
- `Session` - handle to control running capture (stop, stats)
- `FormatPreset::Transcription` - 16kHz mono, optimized for speech-to-text
- `FormatPreset::Native` - use device's native format
- `StreamConfig` - configure chunk duration, buffer size, retry behavior

#### Sinks
- `Sink` trait - implement custom audio destinations
- `FileSink` - write audio to WAV files
- `ChannelSink` - send chunks via `tokio::sync::mpsc`

#### Audio Source
- `AudioDevice` - CPAL device wrapper with automatic format detection
- `MockSource` - synthetic audio generation for testing without hardware
- `list_input_devices()` / `default_input_device_name()` - device enumeration

#### Pipeline
- Lock-free ring buffer (30s default) absorbs pressure from slow sinks
- Router task fans out audio to multiple sinks concurrently
- Retry logic with exponential backoff for transient sink failures

#### Format Conversion
- `f32_to_i16` / `i16_to_f32` - sample format conversion
- `stereo_to_mono` - channel downmixing
- `resample()` - linear interpolation resampling

#### Error Handling
- `StreamAudioError` - fatal errors that prevent capture from starting
- `SinkError` - recoverable errors within sink implementations
- `StreamEvent` - runtime events (buffer overflow, sink errors) via callback

#### Examples
- `simple_record` - minimal recording to WAV file
- `multi_sink` - file + channel simultaneously
- `custom_sink` - implementing the Sink trait

### Architecture

```
CPAL AUDIO THREAD (high-priority, never blocks)
  └── CPAL Callback → Ring Buffer (30s default)
                          │
                          ▼ (async recv)
TOKIO RUNTIME
  └── Router Task → FileSink, ChannelSink, CustomSink...
```

**Key invariant:** The CPAL callback never waits. If sinks are slow, the ring buffer absorbs pressure.

### Technical Decisions
- **Async Runtime:** Tokio (fully embraced, no abstraction layer)
- **Ring Buffer:** `ringbuf` crate for lock-free SPSC queue
- **Error Philosophy:** Fatal errors at start(), recoverable events via callback
- **Thread Safety:** All sinks must be `Send + Sync`

### Dependencies
- `cpal` 0.15 - cross-platform audio I/O
- `tokio` 1.x - async runtime
- `ringbuf` 0.4 - lock-free ring buffer
- `async-trait` 0.1 - async trait support
- `thiserror` 2 - error derive macros

[0.1.0]: https://github.com/lotl-co/stream-audio/releases/tag/v0.1.0
