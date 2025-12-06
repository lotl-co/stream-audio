# AI Assistant Guidelines

This document provides guidance for AI assistants (Claude, Copilot, Cursor, etc.) when working with this codebase.

## Project Overview

`stream-audio` is a Rust crate for real-time audio capture with a multi-sink architecture. It provides non-blocking audio capture via CPAL with multiple simultaneous destinations (file, channel, custom) and resilient buffering.

**Repository:** https://github.com/lotl-co/stream-audio

## Architecture Constraints

### Thread Boundary (Critical)

The CPAL audio callback runs on a high-priority OS thread and **must never block**. All blocking operations happen in the Tokio runtime.

```
CPAL Thread (never blocks) → Ring Buffer → Tokio Runtime → Sinks
```

### Technical Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Async Runtime | Tokio | Industry standard, fully embraced |
| Ring Buffer | `ringbuf` crate | Lock-free SPSC queue |
| Audio I/O | CPAL | Cross-platform audio |
| Error Handling | `thiserror` | Derive macro for errors |

## Development Practices

### Test-Driven Development (TDD)

1. Write failing tests first
2. Implement minimal code to pass
3. Refactor while keeping tests green

```bash
cargo test                     # Run all tests
cargo test test_name           # Run specific test
cargo test -- --nocapture      # Show println output
```

### Domain-Driven Design (DDD)

Core domain terms:

| Term | Definition |
|------|------------|
| **Source** | Audio input device (microphone, system audio) |
| **Sink** | Destination for audio (file, channel, network) |
| **Chunk** | Buffer of samples with metadata |
| **Session** | Recording session from start() to stop() |

### SOLID Principles

- **S**: Each module has a single responsibility (source/, sink/, pipeline/)
- **O**: `Sink` trait is open for extension, closed for modification
- **L**: All `Sink` implementations are substitutable
- **I**: Small, focused traits (`Sink` only requires `write()`)
- **D**: Depend on `Sink` trait, not concrete implementations

### Clean Code

- Functions should do one thing
- Use descriptive names over comments
- Keep functions small and focused
- Handle errors explicitly (no `.unwrap()` in library code)

## Code Style

### Formatting and Linting

```bash
cargo fmt                      # Format code
cargo clippy                   # Run lints
cargo clippy --fix             # Auto-fix where possible
```

### Error Handling

- **Fatal errors** (`StreamAudioError`): Returned from `start()`, prevent stream from starting
- **Recoverable events** (`StreamEvent`): Emitted via callback, stream continues

```rust
// Good: Explicit error handling
let device = get_device().map_err(|e| StreamAudioError::DeviceNotFound {
    name: device_name.to_string()
})?;

// Bad: Panics in library code
let device = get_device().unwrap();
```

### Documentation

Every public item needs documentation. Balance this with clean code comments explaining "why" over "what".

```rust
/// A destination for audio data.
///
/// # Example
///
/// ```rust
/// use stream_audio::Sink;
///
/// struct MySink;
///
/// impl Sink for MySink {
///     // ...
/// }
/// ```
pub trait Sink: Send + Sync {
    // ...
}
```

## Module Structure

```
src/
├── lib.rs           # Public API, re-exports
├── builder.rs       # StreamAudio::builder()
├── session.rs       # Session lifecycle
├── config.rs        # StreamConfig, FormatPreset
├── error.rs         # StreamAudioError, SinkError
├── event.rs         # StreamEvent, EventCallback
├── chunk.rs         # AudioChunk struct
├── source/          # Audio input devices
├── sink/            # Sink trait and implementations
├── pipeline/        # Ring buffer and router
└── format/          # Resampling and conversion
```

## Common Commands

```bash
cargo build                    # Build
cargo test                     # Test
cargo doc --open               # Documentation
cargo clippy                   # Lint
cargo fmt                      # Format
cargo bench                    # Benchmarks (when available)
```

## What NOT to Do

- Never block in the CPAL callback thread
- Never use `.unwrap()` or `.expect()` in library code
- Never ignore clippy warnings without explicit `#[allow(...)]` with justification
- Never commit without running `cargo fmt` and `cargo clippy`
- Never add dependencies without considering the compile-time impact
