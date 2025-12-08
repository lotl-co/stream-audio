//! # stream-audio
//!
//! **Note:** This crate is under active development. The API may change before 1.0.
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
//! use stream_audio::{StreamAudio, AudioSource, FileSink, ChannelSink, FormatPreset};
//! use tokio::sync::mpsc;
//!
//! // User channel capacity: size based on your consumer's processing speed
//! let (tx, rx) = mpsc::channel::<AudioChunk>(32);
//!
//! let session = StreamAudio::builder()
//!     .add_source("mic", AudioSource::default_device())
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

// unsafe_code lint is configured in Cargo.toml as "deny" to allow system-audio module override
#![warn(missing_docs)]
// Audio code requires intentional numeric casts between sample formats
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless
)]
// unwrap/expect allowed in tests only (per AGENTS.md)
#![allow(clippy::unwrap_used)]
// These doc lints are too strict for internal implementation details
#![allow(clippy::missing_panics_doc, clippy::missing_errors_doc)]

mod builder;
mod chunk;
mod config;
mod error;
mod event;
pub mod format;
mod pipeline;
mod session;
mod sink;
pub mod source;

pub use builder::{AudioSource, StreamAudio, StreamAudioBuilder};
pub use chunk::AudioChunk;
pub use config::{FormatPreset, StreamConfig};
pub use error::{SinkError, StreamAudioError};
pub use event::{event_callback, EventCallback, StreamEvent};
pub use session::{Session, SessionStats};
pub use sink::{ChannelSink, FileSink, Sink};
pub use source::{
    default_input_device_name, list_input_devices, AudioDevice, DeviceConfig, MockSource, SourceId,
};

// Re-export ScreenCaptureKit types when feature is enabled
#[cfg(feature = "screencapturekit")]
pub use source::system_audio::{
    AppIdentifier, CaptureTarget, ScreenCaptureKitConfig, SystemAudioEvent,
};
