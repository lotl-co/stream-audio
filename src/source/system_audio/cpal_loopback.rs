//! CPAL loopback backend for system audio capture on macOS.
//!
//! Uses Core Audio Taps (macOS 14.2+) via CPAL's loopback support to capture
//! system audio output without requiring virtual audio devices.

use ringbuf::HeapCons;

use super::SystemAudioBackend;
use crate::source::{AudioDevice, CaptureStream, LoopbackDevice};
use crate::StreamAudioError;

/// CPAL loopback backend for macOS system audio capture.
///
/// Uses Core Audio Taps to capture the default output device's audio.
/// This is what you hear through speakers/headphones.
pub struct CpalLoopbackBackend {
    device: LoopbackDevice,
}

impl CpalLoopbackBackend {
    /// Creates a new CPAL loopback backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - macOS version < 14.2
    /// - No default output device is available
    pub fn new() -> Result<Self, StreamAudioError> {
        let device = AudioDevice::open_loopback()?;
        Ok(Self { device })
    }
}

impl SystemAudioBackend for CpalLoopbackBackend {
    fn start_capture(&self) -> Result<(CaptureStream, HeapCons<i16>), StreamAudioError> {
        self.device.start_capture()
    }

    fn native_config(&self) -> (u32, u16) {
        // Core Audio typically provides 48kHz stereo, but query the actual device
        self.device.native_config().unwrap_or((48000, 2))
    }

    fn name(&self) -> &'static str {
        "Core Audio Tap (CPAL)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires macOS 14.2+ with audio hardware"]
    fn test_cpal_loopback_backend_new() {
        let backend = CpalLoopbackBackend::new();
        assert!(backend.is_ok());
    }

    #[test]
    #[ignore = "requires macOS 14.2+ with audio hardware"]
    fn test_cpal_loopback_capture() {
        let backend = CpalLoopbackBackend::new().unwrap();
        let (sample_rate, channels) = backend.native_config();
        // Core Audio typically uses 48kHz stereo
        assert!(sample_rate > 0);
        assert!(channels > 0);

        let result = backend.start_capture();
        assert!(result.is_ok());
    }
}
