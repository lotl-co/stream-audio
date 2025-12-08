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
            if let Ok(desc) = device.description() {
                if desc.name() == name {
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

    /// Opens the default output device for loopback capture (system audio).
    ///
    /// This captures what's playing through speakers/headphones using
    /// Core Audio Taps. Requires macOS 14.2+.
    ///
    /// # Errors
    ///
    /// Returns `SystemAudioUnavailable` if no default output device exists.
    #[cfg(all(target_os = "macos", feature = "system-audio"))]
    pub fn open_loopback() -> Result<LoopbackDevice, StreamAudioError> {
        let host = cpal::default_host();
        let device =
            host.default_output_device()
                .ok_or(StreamAudioError::SystemAudioUnavailable {
                    reason: "no default output device for loopback".into(),
                })?;

        Ok(LoopbackDevice {
            device,
            config: DeviceConfig::default(),
        })
    }

    /// Sets the device configuration.
    pub fn with_config(mut self, config: DeviceConfig) -> Self {
        self.config = config;
        self
    }

    /// Returns the device name.
    pub fn name(&self) -> String {
        self.device
            .description()
            .map_or_else(|_| "unknown".to_string(), |d| d.name().to_string())
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

        Ok((CaptureStream::from_cpal(stream), consumer))
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

/// Wrapper around a CPAL output device for loopback capture.
///
/// Uses Core Audio Taps (macOS 14.2+) to capture system audio output.
/// Unlike `AudioDevice`, this queries output config since the underlying
/// device is an output device that CPAL will tap for input.
#[cfg(all(target_os = "macos", feature = "system-audio"))]
#[must_use]
pub struct LoopbackDevice {
    device: Device,
    config: DeviceConfig,
}

#[cfg(all(target_os = "macos", feature = "system-audio"))]
impl LoopbackDevice {
    /// Returns the device's native output format (sample rate, channels).
    pub fn native_config(&self) -> Result<(u32, u16), StreamAudioError> {
        let config = self
            .device
            .default_output_config()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;
        Ok((config.sample_rate().0, config.channels()))
    }

    /// Starts capturing loopback audio and returns a running stream.
    pub fn start_capture(
        &self,
    ) -> Result<(CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        let ring_buffer = HeapRb::<i16>::new(self.config.buffer_capacity);
        let (producer, consumer) = ring_buffer.split();

        // Get OUTPUT config (this is an output device we're tapping)
        let supported_config = self
            .device
            .default_output_config()
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        let sample_format = supported_config.sample_format();
        let cpal_config: CpalStreamConfig = supported_config.into();

        // Build INPUT stream on OUTPUT device - CPAL creates loopback tap internally
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

        Ok((CaptureStream::from_cpal(stream), consumer))
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
                    let _ = producer.push_slice(data);
                },
                |err| {
                    tracing::error!("Loopback stream error: {}", err);
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
                    for &sample in data {
                        let converted =
                            (sample * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16;
                        let _ = producer.try_push(converted);
                    }
                },
                |err| {
                    tracing::error!("Loopback stream error: {}", err);
                },
                None,
            )
            .map_err(|e| StreamAudioError::BackendError(e.to_string()))?;

        Ok(stream)
    }
}

/// A running audio capture stream.
///
/// Audio capture continues while this struct is held. When dropped, the
/// underlying stream is automatically stopped and resources are released.
///
/// This is a simple RAII wrapper - the stream runs while this exists.
/// Supports both CPAL device streams and system audio backends.
pub struct CaptureStream {
    /// The underlying stream. Dropping this stops capture.
    /// Field is intentionally never read - it exists only for RAII cleanup.
    #[allow(dead_code)]
    inner: CaptureStreamInner,
}

/// Internal enum to support different stream backends.
/// All variants hold streams for RAII cleanup - fields are never read directly.
#[allow(dead_code)]
enum CaptureStreamInner {
    /// CPAL audio device stream.
    Cpal(Stream),
    /// System audio backend stream (holds boxed trait object for cleanup).
    #[cfg(any(feature = "system-audio", feature = "screencapturekit"))]
    SystemAudio(Box<dyn std::any::Any + Send>),
}

impl CaptureStream {
    /// Create a `CaptureStream` from a CPAL stream.
    pub(crate) fn from_cpal(stream: Stream) -> Self {
        Self {
            inner: CaptureStreamInner::Cpal(stream),
        }
    }

    /// Create a `CaptureStream` from a system audio backend.
    /// Used by mock backend for testing; may be used by future non-CPAL backends.
    #[cfg(any(feature = "system-audio", feature = "screencapturekit"))]
    #[allow(dead_code)]
    pub(crate) fn from_system_audio<T: Send + 'static>(stream: T) -> Self {
        Self {
            inner: CaptureStreamInner::SystemAudio(Box::new(stream)),
        }
    }
}

// CaptureStream uses RAII - stream runs while it exists, stops when dropped.
// No explicit stop() needed; the underlying stream handles cleanup on drop.

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
