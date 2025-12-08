//! System audio capture backend abstraction.
//!
//! This module provides platform-specific system audio capture (speaker/output audio)
//! via native APIs like `ScreenCaptureKit` on macOS.

#[cfg(all(target_os = "macos", feature = "system-audio"))]
mod screencapturekit;

#[cfg(test)]
pub mod mock;

use ringbuf::HeapCons;

use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Default sample rate for system audio capture (48kHz).
/// `ScreenCaptureKit` typically provides audio at this rate.
pub const DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE: u32 = 48000;

/// Default channel count for system audio capture (stereo).
pub const DEFAULT_SYSTEM_AUDIO_CHANNELS: u16 = 2;

/// Ring buffer capacity for system audio (30 seconds at 48kHz stereo).
const SYSTEM_AUDIO_BUFFER_CAPACITY: usize = 48000 * 2 * 30;

/// Backend for capturing system audio.
///
/// Implementations provide platform-specific system audio capture,
/// returning the same `(CaptureStream, HeapCons<i16>)` interface
/// as device capture for seamless integration with the pipeline.
pub trait SystemAudioBackend: Send {
    /// Start capturing system audio.
    ///
    /// Returns a capture stream handle (keeps capture alive) and
    /// a ring buffer consumer for reading audio samples.
    fn start_capture(&self) -> Result<(CaptureStream, HeapCons<i16>), StreamAudioError>;

    /// Returns the native sample rate and channel count.
    fn native_config(&self) -> (u32, u16);

    /// Backend name for logging/debugging.
    fn name(&self) -> &'static str;
}

/// Creates the appropriate system audio backend for the current platform.
///
/// # Errors
///
/// Returns `SystemAudioUnavailable` if:
/// - The `system-audio` feature is not enabled
/// - The platform doesn't support system audio capture
/// - Required permissions are not granted
#[allow(unreachable_code)]
pub fn create_system_audio_backend() -> Result<Box<dyn SystemAudioBackend>, StreamAudioError> {
    #[cfg(all(target_os = "macos", feature = "system-audio"))]
    {
        return Ok(Box::new(screencapturekit::ScreenCaptureKitBackend::new()?));
    }

    #[cfg(all(target_os = "macos", not(feature = "system-audio")))]
    {
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "system-audio feature not enabled - add `features = [\"system-audio\"]` to Cargo.toml".to_string(),
        });
    }

    #[cfg(target_os = "windows")]
    {
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "Windows system audio capture not yet implemented".to_string(),
        });
    }

    #[cfg(target_os = "linux")]
    {
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "Linux system audio capture not yet implemented".to_string(),
        });
    }

    Err(StreamAudioError::SystemAudioUnavailable {
        reason: "system audio capture not supported on this platform".to_string(),
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
    #[cfg(not(feature = "system-audio"))]
    fn test_create_backend_without_feature() {
        let result = create_system_audio_backend();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(
            err,
            StreamAudioError::SystemAudioUnavailable { .. }
        ));
    }
}
