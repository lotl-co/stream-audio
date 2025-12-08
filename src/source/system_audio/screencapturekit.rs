//! `ScreenCaptureKit` backend for system audio capture on macOS.
//!
//! Uses Apple's `ScreenCaptureKit` framework (macOS 13.0+) to capture
//! system audio output without requiring virtual audio devices.

use std::sync::Arc;

use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;
use screencapturekit::prelude::*;

use super::{
    SystemAudioBackend, DEFAULT_SYSTEM_AUDIO_CHANNELS, DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE,
    SYSTEM_AUDIO_BUFFER_CAPACITY,
};
use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Symmetric i16 max for audio conversion (avoids asymmetric clipping).
const I16_MAX_SYMMETRIC: f32 = i16::MAX as f32;
/// Minimum i16 as f32 for clamping.
const I16_MIN_F32: f32 = i16::MIN as f32;
/// Maximum i16 as f32 for clamping.
const I16_MAX_F32: f32 = i16::MAX as f32;

/// ScreenCaptureKit-based system audio backend for macOS.
pub struct ScreenCaptureKitBackend {
    sample_rate: u32,
    channels: u16,
}

impl ScreenCaptureKitBackend {
    /// Creates a new `ScreenCaptureKit` backend.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - macOS version < 13.0
    /// - Screen Recording permission is not granted
    /// - No displays are available
    pub fn new() -> Result<Self, StreamAudioError> {
        // Check if we can access shareable content (requires permission)
        SCShareableContent::get().map_err(|e| {
            if e.to_string().contains("permission")
                || e.to_string().contains("denied")
                || e.to_string().contains("authorized")
            {
                StreamAudioError::SystemAudioPermissionDenied
            } else {
                StreamAudioError::SystemAudioUnavailable {
                    reason: e.to_string(),
                }
            }
        })?;

        Ok(Self {
            sample_rate: DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE,
            channels: DEFAULT_SYSTEM_AUDIO_CHANNELS,
        })
    }
}

impl SystemAudioBackend for ScreenCaptureKitBackend {
    fn start_capture(&self) -> Result<(CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        // Get available content
        let content =
            SCShareableContent::get().map_err(|e| StreamAudioError::SystemAudioUnavailable {
                reason: e.to_string(),
            })?;

        // Get the first display (required for content filter)
        let display = content.displays().into_iter().next().ok_or_else(|| {
            StreamAudioError::SystemAudioUnavailable {
                reason: "no displays available for audio capture".to_string(),
            }
        })?;

        // Create content filter for the display
        let filter = SCContentFilter::builder()
            .display(&display)
            .exclude_windows(&[])
            .build();

        // Configure stream for audio capture
        // Note: SCK requires video config even for audio-only capture
        let config = SCStreamConfiguration::new()
            .with_width(1)
            .with_height(1)
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true) // Prevent feedback loops
            .with_sample_rate(self.sample_rate as i32)
            .with_channel_count(i32::from(self.channels));

        // Create ring buffer for audio samples
        let ring_buffer = HeapRb::<i16>::new(SYSTEM_AUDIO_BUFFER_CAPACITY);
        let (producer, consumer) = ring_buffer.split();

        // Create handler that pushes audio to ring buffer
        let handler = AudioHandler {
            producer: Arc::new(std::sync::Mutex::new(producer)),
        };

        // Create and start stream
        let mut stream = SCStream::new(&filter, &config);
        stream.add_output_handler(handler, SCStreamOutputType::Audio);

        stream
            .start_capture()
            .map_err(|e| StreamAudioError::SystemAudioUnavailable {
                reason: format!("failed to start capture: {e}"),
            })?;

        // Wrap the stream in CaptureStream for RAII cleanup
        let capture_stream = SystemAudioCaptureStream { _stream: stream };

        Ok((CaptureStream::from_system_audio(capture_stream), consumer))
    }

    fn native_config(&self) -> (u32, u16) {
        (self.sample_rate, self.channels)
    }

    fn name(&self) -> &'static str {
        "ScreenCaptureKit"
    }
}

/// Handler that receives audio samples from `ScreenCaptureKit`.
struct AudioHandler {
    producer: Arc<std::sync::Mutex<ringbuf::HeapProd<i16>>>,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        if output_type != SCStreamOutputType::Audio {
            return;
        }

        // Extract audio buffer list
        let Some(buffer_list) = sample.audio_buffer_list() else {
            return;
        };

        // Lock producer and push samples
        let Ok(mut producer) = self.producer.lock() else {
            return;
        };

        // Process each audio buffer in the list
        for buffer in &buffer_list {
            let data = buffer.data();
            if data.is_empty() {
                continue;
            }

            // Audio data is F32 format - interpret bytes as f32 samples
            // Safety: ScreenCaptureKit provides properly aligned f32 audio data
            let sample_count = data.len() / std::mem::size_of::<f32>();
            #[allow(unsafe_code, clippy::cast_ptr_alignment)]
            let samples: &[f32] =
                unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<f32>(), sample_count) };

            for &sample_f32 in samples {
                let converted =
                    (sample_f32 * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16;
                // Non-blocking push - drops samples if buffer is full
                let _ = producer.try_push(converted);
            }
        }
    }
}

/// Wrapper around `SCStream` that provides RAII cleanup.
struct SystemAudioCaptureStream {
    _stream: SCStream,
}

impl Drop for SystemAudioCaptureStream {
    fn drop(&mut self) {
        // SCStream::stop_capture is called automatically when dropped
        // but we could explicitly call it here if needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires macOS with Screen Recording permission"]
    fn test_screencapturekit_backend_new() {
        let backend = ScreenCaptureKitBackend::new();
        assert!(backend.is_ok());
    }

    #[test]
    #[ignore = "requires macOS with Screen Recording permission"]
    fn test_screencapturekit_capture() {
        let backend = ScreenCaptureKitBackend::new().unwrap();
        let (sample_rate, channels) = backend.native_config();
        assert_eq!(sample_rate, DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE);
        assert_eq!(channels, DEFAULT_SYSTEM_AUDIO_CHANNELS);

        let result = backend.start_capture();
        assert!(result.is_ok());
    }
}
