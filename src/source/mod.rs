//! Audio source abstraction and CPAL device wrapper.
//!
//! This module provides the interface between CPAL's low-level audio capture
//! and the rest of the stream-audio pipeline.

mod device;
mod mock;
mod source_id;
#[cfg(feature = "system-audio")]
pub mod system_audio;

pub use device::{AudioDevice, CaptureStream, DeviceConfig};
pub use mock::MockSource;
pub use source_id::SourceId;

use cpal::traits::{DeviceTrait, HostTrait};

/// Lists all available input devices.
///
/// # Errors
///
/// Returns an error if the audio host cannot be accessed.
pub fn list_input_devices() -> Result<Vec<String>, crate::StreamAudioError> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| crate::StreamAudioError::BackendError(e.to_string()))?;

    Ok(devices.filter_map(|d| d.name().ok()).collect())
}

/// Gets the name of the default input device, if any.
pub fn default_input_device_name() -> Option<String> {
    cpal::default_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_devices_doesnt_panic() {
        // This may return empty list in CI, but shouldn't panic
        let _ = list_input_devices();
    }

    #[test]
    fn test_default_device_doesnt_panic() {
        // This may return None in CI, but shouldn't panic
        let _ = default_input_device_name();
    }
}
