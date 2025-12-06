# AI Assistants

Guidelines for using AI tools (Claude, Cursor, Copilot, ChatGPT, etc.) with this repo.


## How to Behave

- Prefer small, focused changes.
- Preserve public API stability unless explicitly told otherwise.
- When unsure, propose changes in comments instead of modifying code directly.
- Never introduce blocking operations into real-time code paths.

## Project Summary

`stream-audio` is a Rust crate for real-time audio capture using CPAL. It uses:
- a non-blocking audio callback on the CPAL thread,
- a ring buffer as a safety valve,
- Tokio tasks that forward audio to one or more sinks (file, channel, custom).

Core flow:

> CPAL audio thread (never blocks) → ring buffer → Tokio runtime → sinks

## Invariants (Do Not Break)

- CPAL callback must **never block**.
- All I/O (file, network, channels) happens **off** the audio thread.
- It is OK to **drop audio under backpressure**; it is NOT OK to stall capture.
- Library code should **not panic** in normal paths (`unwrap`/`expect` are for tests & binaries only).

## Core Concepts

- **Source** – audio input device (e.g. mic, system audio).
- **Sink** – destination for audio (file, channel, network, etc.).
- **Chunk** – small buffer of samples + metadata.
- **Session** – a running capture from `start()` to `stop()`.

## Technical Decisions

- Async runtime: **Tokio** (no other runtimes).
- Ring buffer: **`ringbuf`** crate.
- ChannelSink: **`tokio::sync::mpsc::Sender<AudioChunk>`**.
- Error model:
  - Fatal errors (`StreamAudioError`) stop `start()`.
  - Recoverable issues are surfaced as runtime events; the stream should continue where possible.

## Good Tasks for AI

- Implement new sinks (e.g. TCP/UDP/in-memory sinks).
- Add tests using mock sources/sinks.
- Improve documentation, examples, and error messages.
- Refactor internals without changing the public API or invariants above.

## Boundaries (Do Not Touch)

- CPAL callback thread behavior or timing.
- Public API signatures (unless asked).
- Threading model (CPAL thread → ring buffer → Tokio).
- Any change that would make capture blocking or introduce locks in the callback.

## Handy Commands

```bash
cargo build
cargo test
cargo doc --open
cargo clippy
cargo fmt
```
