//! # stream-audio
//!
//! **Note:** This crate is a work in progress and currently does nothing functional.
//! It exists to reserve the crate name. Check back soon!
//!
//! Real-time audio capture with multi-sink architecture.
//!
//! `stream-audio` provides non-blocking audio capture via CPAL with multiple
//! simultaneous destinations (file, channel, custom) and resilient buffering
//! that never drops audio due to slow consumers.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use stream_audio::{StreamAudio, FileSink, ChannelSink, FormatPreset};
//! use tokio::sync::mpsc;
//!
//! let (tx, rx) = mpsc::channel::<AudioChunk>(100);
//!
//! let session = StreamAudio::builder()
//!     .format(FormatPreset::Transcription)           // 16kHz mono
//!     .add_sink(FileSink::wav("meeting.wav"))
//!     .add_sink(ChannelSink::new(tx))
//!     .on_event(|e| tracing::warn!(?e, "stream event"))
//!     .start()
//!     .await?;
//!
//! // Process chunks as they arrive
//! while let Some(chunk) = rx.recv().await {
//!     // Send to Deepgram, Whisper, etc.
//! }
//!
//! session.stop().await?;
//! ```
//!
//! ## Architecture
//!
//! The crate maintains a strict thread boundary:
//!
//! - **CPAL Thread**: High-priority audio callback that never blocks
//! - **Ring Buffer**: Lock-free SPSC queue absorbs pressure from slow consumers
//! - **Tokio Runtime**: Async router fans out to all registered sinks
//!
//! This design ensures audio capture is never interrupted by slow file I/O,
//! network latency, or processing delays.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod chunk;
mod config;
mod error;
mod event;
mod pipeline;
mod sink;
pub mod source;

pub use chunk::AudioChunk;
pub use config::{FormatPreset, StreamConfig};
pub use error::{SinkError, StreamAudioError};
pub use event::{event_callback, EventCallback, StreamEvent};
pub use sink::{ChannelSink, FileSink, Sink};
pub use source::{default_input_device_name, list_input_devices, AudioDevice, DeviceConfig, MockSource};
