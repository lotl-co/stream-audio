# Contributing to stream-audio

Thank you for your interest in contributing to `stream-audio`! This document outlines our development practices and how to get started.

## Getting Started

### Prerequisites

- Rust 1.75 or later
- [just](https://github.com/casey/just) command runner
- An audio input device (for manual testing)

```bash
# Install just
cargo install just
# or: brew install just
# or: apt install just
```

### Setup

```bash
git clone https://github.com/lotl-co/stream-audio.git
cd stream-audio
cargo build
just check  # Run all checks
```

## Development Practices

We follow these practices to maintain code quality:

### Test-Driven Development (TDD)

1. **Red**: Write a failing test that defines the expected behavior
2. **Green**: Write the minimal code to make the test pass
3. **Refactor**: Improve the code while keeping tests green

```bash
# Run tests continuously during development
cargo watch -x test

# Run a specific test
cargo test test_chunk_creation

# Run tests with output
cargo test -- --nocapture
```

### Domain-Driven Design (DDD)

Use the domain language consistently:

| Term    | Meaning                  |
|---------|--------------------------|
| Source  | Audio input device       |
| Sink    | Audio destination        |
| Chunk   | Buffer of audio samples  |
| Session | Active recording session |

### SOLID Principles
Ò
- **Single Responsibility**: One reason to change per module
- **Open/Closed**: Extend via traits, not modification
- **Liskov Substitution**: All Sink implementations are interchangeable
- **Interface Segregation**: Small, focused traits
- **Dependency Inversion**: Depend on abstractions (traits)

### Clean Code

- Descriptive names over comments
- Small, focused functions
- Explicit error handling
- No magic numbers or strings

## Code Quality Checks

Before submitting a PR, ensure all checks pass:

```bash
just check
```

This runs:
- `cargo fmt -- --check` - Format check
- `cargo clippy -- -D warnings` - Lints (must pass with no warnings)
- `cargo test` - All tests
- `cargo doc --no-deps` - Documentation builds

## Architecture Guidelines

### Thread Safety

The CPAL audio callback runs on a high-priority OS thread. **Never block this thread.**

```
✓ Good: Lock-free ring buffer writes
✗ Bad: Mutex locks, file I/O, network calls
```

### Error Handling

Two categories of errors:

1. **Fatal** (`StreamAudioError`): Prevent start, returned as `Result`
2. **Recoverable** (`StreamEvent`): Runtime issues, emitted via callback

```rust
// Fatal: Can't start without a device
StreamAudioError::DeviceNotFound { name }

// Recoverable: Buffer overflow, but we continue
StreamEvent::BufferOverflow { dropped_ms }
```

### No Panics in Library Code

```rust
// ✗ Bad
let device = get_default_device().unwrap();

// ✓ Good
let device = get_default_device()
    .ok_or(StreamAudioError::NoDefaultDevice)?;
```

## Pull Request Process

1. **Fork** the repository
2. **Create a branch** from `main` with a descriptive name
3. **Write tests first** (TDD)
4. **Implement** your changes
5. **Run `just check`** to verify all checks pass
6. **Submit PR** with a clear description

### PR Checklist

- [ ] Tests added/updated
- [ ] Documentation updated
- [ ] `just check` passes

### Commit Messages

Use clear, descriptive commit messages:

```
feat(sink): add flush() method to FileSink

- Allows explicit buffer flushing before stop()
- Useful for ensuring data is written on graceful shutdown
```

Format: `type(scope): description`

Types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`

## Testing

### Unit Tests

Test individual components in isolation:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_duration() {
        let chunk = AudioChunk {
            samples: vec![0i16; 1600],
            sample_rate: 16000,
            channels: 1,
            timestamp: Duration::ZERO,
        };
        assert_eq!(chunk.duration(), Duration::from_millis(100));
    }
}
```

### Integration Tests

Test components working together:

```rust
// tests/integration.rs
#[tokio::test]
async fn test_file_sink_writes_wav() {
    // Setup
    let temp_file = tempfile::NamedTempFile::new().unwrap();
    let sink = FileSink::wav(temp_file.path());

    // Execute
    sink.on_start().await.unwrap();
    sink.write(&test_chunk()).await.unwrap();
    sink.on_stop().await.unwrap();

    // Verify
    let metadata = std::fs::metadata(temp_file.path()).unwrap();
    assert!(metadata.len() > 44); // WAV header + some data
}
```

### Manual Testing

For tests requiring real audio hardware:

```bash
# Run the simple recording example
cargo run --example simple_record

# Test with specific device
AUDIO_DEVICE="Built-in Microphone" cargo run --example simple_record
```

### Debugging System Audio (macOS)

The native ScreenCaptureKit backend (`sck-native` feature) includes debug logging that can help diagnose audio capture issues.

**Enable debug logging:**

```bash
# Via environment variable (works in release builds)
SCK_AUDIO_DEBUG=1 cargo run --example system_audio --features sck-native

# Debug builds have logging enabled by default
cargo run --example system_audio --features sck-native
```

**What gets logged:**
- First 20 audio callbacks (to verify capture is working)
- Every 50th callback thereafter (~1 log/sec at 48kHz)
- Rejected callbacks when `isRunning=false`

Logs appear in the system log and can be viewed with:
```bash
log stream --predicate 'eventMessage contains "[SCK]"' --level debug
```

## Questions?

- Open an issue for bugs or feature requests
- Start a discussion for questions

We appreciate your contributions!
