//! System audio capture backend abstraction.
//!
//! This module provides platform-specific system audio capture (speaker/output audio)
//! via native APIs. On macOS, uses Core Audio Taps (CPAL loopback) for low-latency capture,
//! or ScreenCaptureKit for per-app audio capture.

#[cfg(all(target_os = "macos", feature = "system-audio"))]
mod cpal_loopback;

#[cfg(all(target_os = "macos", feature = "screencapturekit"))]
mod screencapturekit;

#[cfg(test)]
pub mod mock;

use ringbuf::HeapCons;
use std::fmt;

use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Default sample rate for system audio capture (48kHz).
/// Core Audio typically provides audio at this rate.
pub const DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE: u32 = 48000;

/// Default channel count for system audio capture (stereo).
pub const DEFAULT_SYSTEM_AUDIO_CHANNELS: u16 = 2;

// ============================================================================
// ScreenCaptureKit API Types
// ============================================================================

/// Identifies an application for per-app audio capture.
///
/// Bundle IDs are recommended for stability; app names are best-effort
/// first-match only.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AppIdentifier {
    /// Stable identifier (e.g., "us.zoom.xos", "com.spotify.client").
    BundleId(String),
    /// Best-effort: matches first app with this display name.
    /// Use only for convenience; may match wrong app if names collide.
    Name(String),
}

impl fmt::Display for AppIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppIdentifier::BundleId(id) => write!(f, "bundle:{}", id),
            AppIdentifier::Name(name) => write!(f, "name:{}", name),
        }
    }
}

/// What audio to capture via ScreenCaptureKit.
#[derive(Debug, Clone)]
pub enum CaptureTarget {
    /// Capture audio from all SCK-visible applications.
    /// This is approximately "everything you hear from apps."
    ///
    /// **Note:** This is NOT a true "tap the output device" loopback.
    /// Non-app/system-only sounds or OS restrictions may prevent capture.
    AllApps,
    /// Capture audio from a specific application by bundle ID or name.
    ///
    /// Per-app capture is defined at the application level (bundle ID),
    /// not per-tab or per-window. Finer-grained routing is handled downstream.
    App(AppIdentifier),
}

/// Configuration for the ScreenCaptureKit backend.
#[derive(Debug, Clone)]
pub struct ScreenCaptureKitConfig {
    /// What audio to capture.
    pub target: CaptureTarget,
    /// Whether to exclude this process from capture (default: true).
    /// Set to true to avoid feedback loops when playing captured audio.
    pub exclude_self: bool,
}

impl Default for ScreenCaptureKitConfig {
    fn default() -> Self {
        Self {
            target: CaptureTarget::AllApps,
            exclude_self: true,
        }
    }
}

impl ScreenCaptureKitConfig {
    /// Create config for capturing all apps.
    pub fn all_apps() -> Self {
        Self::default()
    }

    /// Create config for capturing a specific app by bundle ID.
    pub fn app_by_bundle_id(bundle_id: impl Into<String>) -> Self {
        Self {
            target: CaptureTarget::App(AppIdentifier::BundleId(bundle_id.into())),
            exclude_self: true,
        }
    }

    /// Create config for capturing a specific app by name (best-effort).
    pub fn app_by_name(name: impl Into<String>) -> Self {
        Self {
            target: CaptureTarget::App(AppIdentifier::Name(name.into())),
            exclude_self: true,
        }
    }

    /// Set whether to exclude self from capture.
    pub fn with_exclude_self(mut self, exclude: bool) -> Self {
        self.exclude_self = exclude;
        self
    }
}

// ============================================================================
// Runtime Events
// ============================================================================

/// Runtime events from system audio capture.
///
/// These events indicate state changes or issues that don't prevent capture
/// but may affect audio quality or availability. Poll these via
/// `SystemAudioBackend::poll_events()`.
#[derive(Debug, Clone)]
pub enum SystemAudioEvent {
    /// Target application stopped/quit (App mode only).
    /// The stream continues but produces silence.
    AppStopped {
        /// The identifier of the app that stopped.
        app: AppIdentifier,
    },

    /// Default audio output device changed.
    /// Audio routing may have changed; consider restarting capture.
    OutputDeviceChanged,

    /// Capture may be degraded due to OS restrictions or errors.
    /// Examples: screen lock, restricted capture mode, unknown SCK errors.
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

/// Thread-safe event queue for system audio events.
#[derive(Default)]
pub struct EventQueue {
    events: std::sync::Mutex<Vec<SystemAudioEvent>>,
}

impl EventQueue {
    /// Create a new empty event queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an event to the queue.
    pub fn push(&self, event: SystemAudioEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    /// Drain and return all pending events.
    pub fn drain(&self) -> Vec<SystemAudioEvent> {
        self.events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default()
    }
}

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

    /// Returns and drains any pending events since the last call.
    /// Non-blocking, safe to call from any thread.
    /// Default implementation returns empty vec for backends that don't emit events.
    fn poll_events(&self) -> Vec<SystemAudioEvent> {
        Vec::new()
    }
}

/// Creates the appropriate system audio backend for the current platform.
///
/// # Errors
///
/// Returns `SystemAudioUnavailable` if:
/// - The `system-audio` feature is not enabled
/// - The platform doesn't support system audio capture (macOS 14.2+ required)
/// - No default output device is available
#[allow(unreachable_code)]
pub fn create_system_audio_backend() -> Result<Box<dyn SystemAudioBackend>, StreamAudioError> {
    #[cfg(all(target_os = "macos", feature = "system-audio"))]
    {
        return Ok(Box::new(cpal_loopback::CpalLoopbackBackend::new()?));
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

/// Creates a ScreenCaptureKit-based system audio backend.
///
/// This backend provides per-app audio capture on macOS 12.3+.
/// Unlike the CPAL loopback backend, SCK allows capturing specific
/// applications by bundle ID or name.
///
/// # Errors
///
/// Returns an error if:
/// - The `screencapturekit` feature is not enabled
/// - The platform is not macOS
/// - Screen recording permission is not granted
/// - The target app is not found (for App mode)
#[allow(unreachable_code)]
pub fn create_screencapturekit_backend(
    config: ScreenCaptureKitConfig,
) -> Result<Box<dyn SystemAudioBackend>, StreamAudioError> {
    #[cfg(all(target_os = "macos", feature = "screencapturekit"))]
    {
        return Ok(Box::new(
            screencapturekit::ScreenCaptureKitBackend::new(config)?,
        ));
    }

    #[cfg(all(target_os = "macos", not(feature = "screencapturekit")))]
    {
        let _ = config;
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "screencapturekit feature not enabled - add `features = [\"screencapturekit\"]` to Cargo.toml".to_string(),
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(StreamAudioError::SystemAudioUnavailable {
            reason: "ScreenCaptureKit is only available on macOS".to_string(),
        });
    }

    #[allow(unreachable_code)]
    {
        let _ = config;
        Err(StreamAudioError::SystemAudioUnavailable {
            reason: "ScreenCaptureKit not supported on this platform".to_string(),
        })
    }
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
