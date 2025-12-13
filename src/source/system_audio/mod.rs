//! System audio capture backend for macOS.
//!
//! This module provides system audio capture (speaker/output audio) via
//! `ScreenCaptureKit` on macOS 13+. Uses a custom Swift library with C FFI
//! for reliable, debuggable audio capture.
//!
//! # Requirements
//!
//! - macOS 13.0 or later
//! - Screen Recording permission
//! - The `sck-native` feature enabled
//!
//! # Example
//!
//! ```ignore
//! use stream_audio::source::system_audio::create_system_audio_backend;
//!
//! let backend = create_system_audio_backend()?;
//! let (stream, consumer) = backend.start_capture()?;
//! // Read audio samples from consumer...
//! ```

#[cfg(all(target_os = "macos", feature = "sck-native"))]
mod sck_native;

#[cfg(test)]
pub mod mock;

pub mod test_support;

use ringbuf::HeapCons;

use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Default sample rate for system audio capture (48kHz).
pub const DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE: u32 = 48000;

/// Default channel count for system audio capture (stereo).
pub const DEFAULT_SYSTEM_AUDIO_CHANNELS: u16 = 2;

// ============================================================================
// Runtime Events
// ============================================================================

/// Runtime events from system audio capture.
///
/// These events indicate state changes or issues that don't prevent capture
/// but may affect audio quality or availability.
#[derive(Debug, Clone)]
pub enum SystemAudioEvent {
    /// Default audio output device changed.
    /// Audio routing may have changed; consider restarting capture.
    OutputDeviceChanged,

    /// Capture may be degraded due to OS restrictions or errors.
    CaptureDegraded {
        /// Description of the degradation.
        reason: String,
    },

    /// Ring buffer overrun - consumer couldn't keep up with producer.
    /// Some audio frames were dropped.
    Overflow {
        /// Number of frames dropped.
        dropped_frames: u64,
    },
}

/// Backend for capturing system audio.
///
/// Implementations provide platform-specific system audio capture,
/// returning the same `(CaptureStream, HeapCons<i16>)` interface
/// as device capture for seamless integration with the pipeline.
pub trait SystemAudioBackend: Send {
    /// Start capturing system audio.
    ///
    /// Consumes the backend and returns a capture stream handle (keeps capture
    /// alive) and a ring buffer consumer for reading audio samples.
    ///
    /// The backend is moved into the `CaptureStream` to ensure the underlying
    /// resources stay alive for the duration of capture.
    fn start_capture(self: Box<Self>) -> Result<(CaptureStream, HeapCons<i16>), StreamAudioError>;

    /// Returns the native sample rate and channel count.
    fn native_config(&self) -> (u32, u16);

    /// Backend name for logging/debugging.
    fn name(&self) -> &'static str;

    /// Returns and drains any pending events since the last call.
    /// Non-blocking, safe to call from any thread.
    fn poll_events(&self) -> Vec<SystemAudioEvent> {
        Vec::new()
    }
}

/// Creates the system audio backend for the current platform.
///
/// # Errors
///
/// Returns `SystemAudioPermissionDenied` if Screen Recording permission is not granted.
/// Returns `SystemAudioUnavailable` if:
/// - The `sck-native` feature is not enabled
/// - The platform is not macOS
/// - macOS version is below 13.0
#[allow(unreachable_code)]
pub fn create_system_audio_backend() -> Result<Box<dyn SystemAudioBackend>, StreamAudioError> {
    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    {
        use crate::platform::permissions::{
            check_screen_capture_permission, ScreenCapturePermission,
        };

        // Check Screen Recording permission before proceeding
        if check_screen_capture_permission() != ScreenCapturePermission::Granted {
            return Err(StreamAudioError::SystemAudioPermissionDenied);
        }

        return Ok(Box::new(sck_native::SCKNativeBackend::new()?));
    }

    #[cfg(all(target_os = "macos", not(feature = "sck-native")))]
    {
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason:
                "sck-native feature not enabled - add `features = [\"sck-native\"]` to Cargo.toml"
                    .into(),
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "System audio capture is only available on macOS".into(),
        });
    }

    #[allow(unreachable_code)]
    Err(StreamAudioError::SystemAudioUnavailable {
        reason: "System audio capture not supported on this platform".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constants() {
        assert_eq!(DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE, 48000);
        assert_eq!(DEFAULT_SYSTEM_AUDIO_CHANNELS, 2);
    }

    #[test]
    #[cfg(not(feature = "sck-native"))]
    fn test_create_backend_without_feature() {
        let result = create_system_audio_backend();
        assert!(result.is_err());
    }
}
