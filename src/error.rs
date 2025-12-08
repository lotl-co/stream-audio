//! Error types for stream-audio.
//!
//! Errors are split into two categories:
//! - **Fatal errors** ([`StreamAudioError`]): Prevent the stream from starting
//! - **Recoverable events**: Runtime issues surfaced via [`EventCallback`](crate::EventCallback)

use std::path::PathBuf;

/// Fatal errors that prevent an audio stream from starting.
///
/// These errors are returned from [`StreamAudioBuilder::start()`] and indicate
/// that the stream cannot be created. Runtime issues (slow sinks, buffer
/// overflow) are handled via the event callback instead.
///
/// [`StreamAudioBuilder::start()`]: crate::StreamAudioBuilder::start
#[derive(Debug, thiserror::Error)]
pub enum StreamAudioError {
    /// The requested audio device was not found.
    #[error("device not found: {name}")]
    DeviceNotFound {
        /// Name of the device that wasn't found.
        name: String,
    },

    /// The requested device exists but is unavailable.
    #[error("device unavailable: {name} - {reason}")]
    DeviceUnavailable {
        /// Name of the unavailable device.
        name: String,
        /// Reason the device is unavailable.
        reason: String,
    },

    /// No default input device is configured on this system.
    #[error("no default input device configured")]
    NoDefaultDevice,

    /// Permission to capture audio was denied.
    ///
    /// On macOS, check System Preferences > Security & Privacy > Microphone.
    #[error("permission denied for audio capture (check OS settings)")]
    PermissionDenied,

    /// The requested sample format is not supported by the device.
    #[error("unsupported sample format: {format}")]
    UnsupportedFormat {
        /// The format that wasn't supported.
        format: String,
    },

    /// The requested sample rate is not supported by the device.
    #[error("sample rate {requested}Hz not supported (available: {available:?})")]
    UnsupportedSampleRate {
        /// The requested sample rate.
        requested: u32,
        /// Sample rates that are supported.
        available: Vec<u32>,
    },

    /// No sinks were configured before starting.
    #[error("no sinks configured - add at least one sink")]
    NoSinksConfigured,

    /// A sink failed during initialization.
    #[error("sink '{sink_name}' failed to start: {reason}")]
    SinkStartFailed {
        /// Name of the sink that failed.
        sink_name: String,
        /// Why the sink failed to start.
        reason: String,
    },

    /// An error from the underlying audio library (CPAL).
    #[error("audio backend error: {0}")]
    BackendError(String),

    /// No sources were configured before starting (multi-source mode).
    #[error("no sources configured - use add_source() to add at least one audio source before calling start()")]
    NoSourcesConfigured,

    /// A sink route references an unknown source ID.
    #[error("unknown source in route: {source_id}")]
    UnknownSourceInRoute {
        /// The source ID that wasn't found.
        source_id: String,
    },

    /// A source ID was used more than once.
    #[error("duplicate source ID: {source_id}")]
    DuplicateSourceId {
        /// The duplicated source ID.
        source_id: String,
    },

    /// System audio capture is not available on this platform or configuration.
    #[error("system audio capture unavailable: {reason}")]
    SystemAudioUnavailable {
        /// Why system audio is unavailable.
        reason: String,
    },

    /// Permission to capture system audio was denied.
    ///
    /// On macOS, check System Preferences > Security & Privacy > Screen Recording.
    #[error("system audio permission denied (check Screen Recording in System Preferences)")]
    SystemAudioPermissionDenied,
}

/// Errors that can occur within a [`Sink`](crate::Sink) implementation.
///
/// Sink errors are recoverable - the router will emit a [`StreamEvent::SinkError`]
/// and may retry the operation.
///
/// [`StreamEvent::SinkError`]: crate::StreamEvent::SinkError
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// A write operation failed.
    #[error("write failed: {reason}")]
    WriteFailed {
        /// Description of what went wrong.
        reason: String,
    },

    /// File I/O error.
    #[error("file error: {path}: {source}")]
    FileError {
        /// Path to the file.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The receiving channel was closed.
    #[error("channel closed")]
    ChannelClosed,

    /// The sink was used before initialization.
    #[error("sink not initialized (call on_start first)")]
    NotInitialized,

    /// Custom error for user-implemented sinks.
    #[error("{0}")]
    Custom(String),
}

impl SinkError {
    /// Creates a custom sink error with the given message.
    pub fn custom(msg: impl Into<String>) -> Self {
        Self::Custom(msg.into())
    }

    /// Creates a write failed error with the given reason.
    pub fn write_failed(reason: impl Into<String>) -> Self {
        Self::WriteFailed {
            reason: reason.into(),
        }
    }

    /// Creates a file error for the given path.
    pub fn file_error(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::FileError {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_audio_error_display() {
        let err = StreamAudioError::DeviceNotFound {
            name: "USB Mic".to_string(),
        };
        assert_eq!(err.to_string(), "device not found: USB Mic");
    }

    #[test]
    fn test_sink_error_custom() {
        let err = SinkError::custom("something went wrong");
        assert_eq!(err.to_string(), "something went wrong");
    }

    #[test]
    fn test_sink_error_write_failed() {
        let err = SinkError::write_failed("buffer full");
        assert_eq!(err.to_string(), "write failed: buffer full");
    }

    #[test]
    fn test_sink_error_file_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = SinkError::file_error("/tmp/test.wav", io_err);
        assert!(err.to_string().contains("/tmp/test.wav"));
    }
}
