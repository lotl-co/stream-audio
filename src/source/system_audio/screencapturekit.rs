//! ScreenCaptureKit backend for per-app system audio capture on macOS.
//!
//! Uses ScreenCaptureKit (macOS 12.3+) to capture audio from specific
//! applications or all apps. Unlike Core Audio Taps, SCK allows targeting
//! individual apps by bundle ID.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use ringbuf::traits::{Producer, Split};
use ringbuf::{HeapProd, HeapRb};
use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::shareable_content::{SCRunningApplication, SCShareableContent};
use screencapturekit::stream::content_filter::SCContentFilter;
use screencapturekit::stream::sc_stream::SCStream;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::output_trait::SCStreamOutputTrait;
use screencapturekit::stream::output_type::SCStreamOutputType;

use super::{
    AppIdentifier, CaptureTarget, EventQueue, ScreenCaptureKitConfig, SystemAudioBackend,
    SystemAudioEvent, DEFAULT_SYSTEM_AUDIO_CHANNELS, DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE,
};
use crate::source::CaptureStream;
use crate::StreamAudioError;

/// Symmetric i16 max for audio conversion (avoids asymmetric clipping).
const I16_MAX_SYMMETRIC: f32 = i16::MAX as f32;
/// Minimum i16 as f32 for clamping.
const I16_MIN_F32: f32 = i16::MIN as f32;
/// Maximum i16 as f32 for clamping.
const I16_MAX_F32: f32 = i16::MAX as f32;

/// Ring buffer capacity: 30 seconds of stereo 48kHz audio.
const BUFFER_CAPACITY: usize = DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE as usize
    * DEFAULT_SYSTEM_AUDIO_CHANNELS as usize
    * 30;

/// ScreenCaptureKit backend for macOS app-level audio capture.
pub struct ScreenCaptureKitBackend {
    config: ScreenCaptureKitConfig,
    event_queue: Arc<EventQueue>,
}

impl ScreenCaptureKitBackend {
    /// Creates a new ScreenCaptureKit backend with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Screen recording permission is not granted
    /// - The target app is not found (for App mode)
    pub fn new(config: ScreenCaptureKitConfig) -> Result<Self, StreamAudioError> {
        // Validate that we can access SCShareableContent (requires permission)
        let _content = SCShareableContent::get().map_err(|e| {
            if e.to_string().contains("permission") {
                StreamAudioError::SystemAudioPermissionDenied
            } else {
                StreamAudioError::SystemAudioUnavailable {
                    reason: format!("failed to access ScreenCaptureKit: {e}"),
                }
            }
        })?;

        Ok(Self {
            config,
            event_queue: Arc::new(EventQueue::new()),
        })
    }

    /// Builds the SCContentFilter based on the capture target.
    fn build_filter(&self) -> Result<SCContentFilter, StreamAudioError> {
        let content = SCShareableContent::get().map_err(|e| {
            StreamAudioError::SystemAudioRuntimeFailure {
                context: "get shareable content".into(),
                cause: e.to_string(),
            }
        })?;

        // Get the main display (required for filter construction)
        let displays = content.displays();
        let display = displays.first().ok_or(StreamAudioError::SystemAudioUnavailable {
            reason: "no displays available".into(),
        })?;

        let apps = content.applications();
        // Get current process bundle ID for self-exclusion
        // This is a simplified approach - full implementation would read from Info.plist
        let current_bundle_id: Option<String> = std::env::current_exe()
            .ok()
            .and_then(|p| {
                p.to_str().and_then(|s| {
                    if s.contains(".app") {
                        // For now, just return None - exclude_self won't work for app bundles
                        // Full implementation would read CFBundleIdentifier from Info.plist
                        None
                    } else {
                        None
                    }
                })
            });

        match &self.config.target {
            CaptureTarget::AllApps => {
                // Include all apps, optionally excluding self
                let apps_to_include: Vec<_> = if self.config.exclude_self {
                    apps.iter()
                        .filter(|app| {
                            current_bundle_id
                                .as_ref()
                                .map(|id| app.bundle_identifier() != *id)
                                .unwrap_or(true)
                        })
                        .collect()
                } else {
                    apps.iter().collect()
                };

                Ok(SCContentFilter::builder()
                    .display(display)
                    .include_applications(&apps_to_include, &[])
                    .build())
            }
            CaptureTarget::App(identifier) => {
                let app = self.find_app(&apps, identifier)?;
                Ok(SCContentFilter::builder()
                    .display(display)
                    .include_applications(&[app], &[])
                    .build())
            }
        }
    }

    /// Finds an application matching the given identifier.
    fn find_app<'a>(
        &self,
        apps: &'a [SCRunningApplication],
        identifier: &AppIdentifier,
    ) -> Result<&'a SCRunningApplication, StreamAudioError>
    {
        match identifier {
            AppIdentifier::BundleId(bundle_id) => apps
                .iter()
                .find(|app| app.bundle_identifier() == *bundle_id)
                .ok_or_else(|| StreamAudioError::SystemAudioAppNotFound {
                    identifier: identifier.to_string(),
                }),
            AppIdentifier::Name(name) => apps
                .iter()
                .find(|app| app.application_name() == *name)
                .ok_or_else(|| StreamAudioError::SystemAudioAppNotFound {
                    identifier: identifier.to_string(),
                }),
        }
    }
}

impl SystemAudioBackend for ScreenCaptureKitBackend {
    fn start_capture(
        &self,
    ) -> Result<(CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        let filter = self.build_filter()?;

        // Configure for audio-only capture
        let stream_config = SCStreamConfiguration::new()
            .with_width(1) // Minimal video (can't disable entirely)
            .with_height(1)
            .with_captures_audio(true)
            .with_sample_rate(DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE as i32)
            .with_channel_count(DEFAULT_SYSTEM_AUDIO_CHANNELS as i32);

        // Create ring buffer
        let ring_buffer = HeapRb::<i16>::new(BUFFER_CAPACITY);
        let (producer, consumer) = ring_buffer.split();

        // Create handler for audio samples
        let handler = AudioHandler {
            producer: std::sync::Mutex::new(producer),
            event_queue: Arc::clone(&self.event_queue),
            overflow_count: AtomicU64::new(0),
            first_sample_logged: AtomicBool::new(false),
        };

        // Create and start the stream
        let mut stream = SCStream::new(&filter, &stream_config);
        stream.add_output_handler(handler, SCStreamOutputType::Audio);

        stream.start_capture().map_err(|e| {
            let err_str = e.to_string();
            if err_str.contains("permission") || err_str.contains("denied") {
                StreamAudioError::SystemAudioPermissionDenied
            } else {
                StreamAudioError::SystemAudioRuntimeFailure {
                    context: "start capture".into(),
                    cause: err_str,
                }
            }
        })?;

        // Wrap stream for RAII cleanup
        let capture_stream = CaptureStream::from_system_audio(StreamWrapper(stream));

        Ok((capture_stream, consumer))
    }

    fn native_config(&self) -> (u32, u16) {
        (DEFAULT_SYSTEM_AUDIO_SAMPLE_RATE, DEFAULT_SYSTEM_AUDIO_CHANNELS)
    }

    fn name(&self) -> &'static str {
        "ScreenCaptureKit"
    }

    fn poll_events(&self) -> Vec<SystemAudioEvent> {
        self.event_queue.drain()
    }
}

/// Wrapper to ensure SCStream is stopped on drop.
struct StreamWrapper(SCStream);

impl Drop for StreamWrapper {
    fn drop(&mut self) {
        // SCStream should stop automatically, but ensure cleanup
        let _ = self.0.stop_capture();
    }
}

/// Handler for receiving audio samples from SCStream.
struct AudioHandler {
    producer: std::sync::Mutex<HeapProd<i16>>,
    event_queue: Arc<EventQueue>,
    overflow_count: AtomicU64,
    first_sample_logged: AtomicBool,
}

impl SCStreamOutputTrait for AudioHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        // Only process audio buffers
        if !matches!(output_type, SCStreamOutputType::Audio) {
            return;
        }

        // Dev-mode self-check: log first sample format and verify expectations
        if !self.first_sample_logged.swap(true, Ordering::Relaxed) {
            let num_samples = sample.num_samples();
            let total_size = sample.total_sample_size();
            tracing::info!(
                "SCK first audio buffer: {} samples, {} bytes total",
                num_samples,
                total_size
            );

            // Verify expected format: f32 interleaved stereo 48kHz
            // Each stereo frame = 2 channels * 4 bytes = 8 bytes
            let expected_bytes_per_frame = 8;
            if total_size > 0 && num_samples > 0 {
                let bytes_per_sample = total_size / num_samples;
                if bytes_per_sample != expected_bytes_per_frame {
                    tracing::warn!(
                        "SCK unexpected audio format: {} bytes/sample (expected {})",
                        bytes_per_sample,
                        expected_bytes_per_frame
                    );
                    self.event_queue.push(SystemAudioEvent::CaptureDegraded {
                        reason: format!(
                            "unexpected audio format: {} bytes/sample",
                            bytes_per_sample
                        ),
                    });
                }
            }
        }

        // Extract audio samples from CMSampleBuffer
        // The exact method depends on screencapturekit-rs API version
        if let Some(audio_data) = self.extract_audio_samples(&sample) {
            let mut producer = match self.producer.lock() {
                Ok(p) => p,
                Err(_) => {
                    tracing::error!("SCK audio handler: failed to lock producer");
                    return;
                }
            };

            // Push samples to ring buffer, tracking overflow
            let mut overflow = 0u64;
            for &sample_f32 in &audio_data {
                let sample_i16 =
                    (sample_f32 * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16;
                if producer.try_push(sample_i16).is_err() {
                    overflow += 1;
                }
            }

            // Report overflow if any
            if overflow > 0 {
                let total = self.overflow_count.fetch_add(overflow, Ordering::Relaxed) + overflow;
                if total % 1000 == 0 || overflow > 100 {
                    tracing::warn!("SCK audio buffer overflow: {} frames dropped (total: {})", overflow, total);
                    self.event_queue.push(SystemAudioEvent::Overflow {
                        dropped_frames: overflow,
                    });
                }
            }
        }
    }
}

impl AudioHandler {
    /// Extracts f32 audio samples from a CMSampleBuffer.
    ///
    /// ScreenCaptureKit provides audio as f32 samples in little-endian format.
    /// This method extracts all audio buffers and converts to a flat Vec<f32>.
    fn extract_audio_samples(&self, sample: &CMSampleBuffer) -> Option<Vec<f32>> {
        let audio_list = sample.audio_buffer_list()?;

        let mut samples = Vec::new();
        for buffer in audio_list.iter() {
            let data = buffer.data();
            // Audio data is f32 samples in little-endian byte order
            for chunk in data.chunks_exact(4) {
                let sample_f32 = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                samples.push(sample_f32);
            }
        }

        if samples.is_empty() {
            None
        } else {
            Some(samples)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = ScreenCaptureKitConfig::default();
        assert!(matches!(config.target, CaptureTarget::AllApps));
        assert!(config.exclude_self);
    }

    #[test]
    fn test_config_app_by_bundle_id() {
        let config = ScreenCaptureKitConfig::app_by_bundle_id("com.spotify.client");
        assert!(matches!(
            config.target,
            CaptureTarget::App(AppIdentifier::BundleId(ref id)) if id == "com.spotify.client"
        ));
    }

    #[test]
    fn test_config_app_by_name() {
        let config = ScreenCaptureKitConfig::app_by_name("Spotify");
        assert!(matches!(
            config.target,
            CaptureTarget::App(AppIdentifier::Name(ref name)) if name == "Spotify"
        ));
    }

    #[test]
    fn test_app_identifier_display() {
        let bundle = AppIdentifier::BundleId("com.example.app".to_string());
        assert_eq!(bundle.to_string(), "bundle:com.example.app");

        let name = AppIdentifier::Name("Example".to_string());
        assert_eq!(name.to_string(), "name:Example");
    }

    #[test]
    #[ignore = "requires macOS with screen recording permission"]
    fn test_backend_new() {
        let config = ScreenCaptureKitConfig::all_apps();
        let backend = ScreenCaptureKitBackend::new(config);
        assert!(backend.is_ok());
    }

    #[test]
    #[ignore = "requires macOS with screen recording permission"]
    fn test_backend_start_capture() {
        let config = ScreenCaptureKitConfig::all_apps();
        let backend = ScreenCaptureKitBackend::new(config).unwrap();
        let result = backend.start_capture();
        assert!(result.is_ok());
    }
}
