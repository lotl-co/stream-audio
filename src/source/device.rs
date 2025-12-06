//! CPAL device wrapper for audio capture.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig as CpalStreamConfig};
use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;

use crate::StreamAudioError;

/// Symmetric i16 max for audio conversion (avoids asymmetric clipping).
const I16_MAX_SYMMETRIC: f32 = i16::MAX as f32;
/// Minimum i16 as f32 for clamping.
const I16_MIN_F32: f32 = i16::MIN as f32;
/// Maximum i16 as f32 for clamping.
const I16_MAX_F32: f32 = i16::MAX as f32;

/// Configuration for audio capture.
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// Target sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u16,
    /// Ring buffer capacity in samples.
    pub buffer_capacity: usize,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            // 30 seconds at 16kHz mono
            buffer_capacity: 16000 * 30,
        }
    }
}

/// Wrapper around a CPAL audio input device.
///
/// This handles device selection, stream configuration, and provides
/// the ring buffer producer for the audio callback.
#[must_use]
pub struct AudioDevice {
    device: Device,
    config: DeviceConfig,
}

impl AudioDevice {
    /// Opens the default input device.
    ///
    /// # Errors
    ///
    /// Returns `NoDefaultDevice` if no default input device is configured.
    pub fn open_default() -> Result<Self, StreamAudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(StreamAudioError::NoDefaultDevice)?;

        Ok(Self {
            device,
            config: DeviceConfig::default(),
        })
    }

    /// Opens a specific input device by name.
    ///
    /// # Errors
    ///
    /// Returns `DeviceNotFound` if no device with the given name exists.
    pub fn open_by_name(name: &str) -> Result<Self, StreamAudioError> {
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        for device in devices {
            if let Ok(device_name) = device.name() {
                if device_name == name {
                    return Ok(Self {
                        device,
                        config: DeviceConfig::default(),
                    });
                }
            }
        }

        Err(StreamAudioError::DeviceNotFound {
            name: name.to_string(),
        })
    }

    /// Sets the device configuration.
    pub fn with_config(mut self, config: DeviceConfig) -> Self {
        self.config = config;
        self
    }

    /// Returns the device name.
    pub fn name(&self) -> String {
        self.device.name().unwrap_or_else(|_| "unknown".to_string())
    }

    /// Returns the current configuration.
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }

    /// Returns the device's native capture format (sample rate, channels).
    ///
    /// This queries CPAL for what the device will actually capture at,
    /// which may differ from the requested format.
    pub fn native_config(&self) -> Result<(u32, u16), StreamAudioError> {
        let config = self
            .device
            .default_input_config()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;
        Ok((config.sample_rate().0, config.channels()))
    }

    /// Starts capturing audio and returns a running stream.
    ///
    /// The returned `CaptureStream` must be kept alive for capture to continue.
    /// Audio samples are pushed to the ring buffer consumer.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream cannot be built or started.
    pub fn start_capture(
        &self,
    ) -> Result<(CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        let ring_buffer = HeapRb::<i16>::new(self.config.buffer_capacity);
        let (producer, consumer) = ring_buffer.split();

        // Get supported config
        let supported_config = self
            .device
            .default_input_config()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        let sample_format = supported_config.sample_format();
        let cpal_config: CpalStreamConfig = supported_config.into();

        // Build stream based on sample format
        let stream = match sample_format {
            SampleFormat::I16 => self.build_i16_stream(&cpal_config, producer)?,
            SampleFormat::F32 => self.build_f32_stream(&cpal_config, producer)?,
            format => {
                return Err(StreamAudioError::UnsupportedFormat {
                    format: format!("{format:?}"),
                });
            }
        };

        stream
            .play()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        Ok((CaptureStream { _stream: stream }, consumer))
    }

    fn build_i16_stream(
        &self,
        config: &CpalStreamConfig,
        mut producer: ringbuf::HeapProd<i16>,
    ) -> Result<Stream, StreamAudioError> {
        let stream = self
            .device
            .build_input_stream(
                config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    // Non-blocking push - drops samples if buffer is full
                    let _ = producer.push_slice(data);
                },
                |err| {
                    tracing::error!("Audio stream error: {}", err);
                },
                None,
            )
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        Ok(stream)
    }

    fn build_f32_stream(
        &self,
        config: &CpalStreamConfig,
        mut producer: ringbuf::HeapProd<i16>,
    ) -> Result<Stream, StreamAudioError> {
        let stream = self
            .device
            .build_input_stream(
                config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Inline conversion to avoid function call overhead in audio callback
                    for &sample in data {
                        let converted =
                            (sample * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16;
                        let _ = producer.try_push(converted);
                    }
                },
                |err| {
                    tracing::error!("Audio stream error: {}", err);
                },
                None,
            )
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        Ok(stream)
    }
}

/// A running audio capture stream.
///
/// Audio capture continues while this struct is held. When dropped, the CPAL
/// stream is automatically stopped and resources are released.
///
/// This is a simple RAII wrapper - the stream runs while this exists.
pub struct CaptureStream {
    /// The underlying CPAL stream. Dropping this stops capture.
    _stream: Stream,
}

// CaptureStream uses RAII - stream runs while it exists, stops when dropped.
// No explicit stop() needed; the CPAL stream handles cleanup on drop.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_config_default() {
        let config = DeviceConfig::default();
        assert_eq!(config.sample_rate, 16000);
        assert_eq!(config.channels, 1);
        assert_eq!(config.buffer_capacity, 16000 * 30);
    }

    // Note: Device tests require actual audio hardware and are skipped in CI
    #[test]
    #[ignore = "requires audio hardware"]
    fn test_open_default_device() {
        let device = AudioDevice::open_default().unwrap();
        println!("Default device: {}", device.name());
    }
}
