# Copilot Repository Instructions for `stream-audio`

These rules tell GitHub Copilot how to behave when suggesting code inside this repository. They summarize our architecture, constraints, and expectations for contributions.

---

## How Copilot Should Behave

- Prefer **small, focused diffs** with minimal churn.
- Preserve **public API stability** unless explicitly told otherwise.
- When uncertain, **propose changes as comments** instead of modifying code.
- **Never introduce blocking operations** into real-time paths.

---

## Core Architecture (Do Not Break)

- The CPAL audio callback must **never block**, allocate excessively, or acquire locks.
- All I/O must occur **off** the audio thread.
- Dropping audio under backpressure is acceptable; stalling capture is not.
- Library code should avoid panics. No `unwrap`/`expect` outside tests or binaries.
- The core pipeline must remain: `CPAL callback → ring buffer → Tokio tasks → sinks`

---

## Domain Concepts

Use these terms consistently:

- **Source** — audio input device
- **Sink** — audio output destination (file, channel, network, etc.)
- **Chunk** — a small buffer of samples with metadata
- **Session** — active capture from `start()` to `stop()`

---

## Technical Rules

- Async runtime: **Tokio only**
- Ring buffer: **`ringbuf` crate**
- Channel Sink: **`tokio::sync::mpsc::Sender<AudioChunk>`**
- Error model:
    - **Fatal**: `StreamAudioError` (prevents `start()`)
    - **Recoverable**: `StreamEvent` (runtime warnings, streaming continues)

---

## Good Tasks for Copilot

- Implementing new sinks (TCP, UDP, in-memory, etc.)
- Adding unit tests and integration tests
- Improving documentation and examples
- Refactoring internals **without altering** public APIs or invariants

---

## Boundaries Copilot Must Not Touch

- CPAL callback timing or behavior
- Public API signatures (unless explicitly requested)
- Concurrency model: CPAL thread → ring buffer → Tokio runtime
- Any code that would add blocking behavior or locks to the audio callback

---

## Code Quality Expectations

- Use descriptive names and small, focused functions
- Prefer explicit error handling over panics
- Follow SOLID where appropriate
- Use domain language consistently

---

## Helpful Commands
```
cargo build
cargo test
cargo clippy
cargo fmt
cargo doc --no-deps
```

---

These guidelines ensure that suggestions from Copilot align with the performance-critical and real-time nature of `stream-audio`.
