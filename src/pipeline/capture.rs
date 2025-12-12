//! Capture bridge task - reads from ring buffer, converts format, sends to router.
//!
//! This module extracts the capture bridge logic from the builder into a dedicated
//! struct with clear responsibilities:
//! - Reading raw audio from the ring buffer (device format)
//! - Converting to target format (resampling + channel conversion)
//! - Sending chunks to the router with proper timestamps
//! - Monitoring audio flow and emitting events when audio stops/resumes

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::event::EventCallback;
use crate::format::FormatConverter;
use crate::pipeline::AudioBuffer;
use crate::session::SessionState;
use crate::source::SourceId;
use crate::{AudioChunk, StreamEvent};

/// Threshold for detecting audio flow stoppage.
/// If no non-zero samples are received for this duration, emit `AudioFlowStopped`.
const SILENCE_THRESHOLD: Duration = Duration::from_millis(500);

/// Monitors audio flow and detects when audio stops/resumes.
struct FlowMonitor {
    /// Time when we last received non-zero audio samples.
    last_audio_time: Instant,
    /// Whether audio is currently flowing (has non-zero samples).
    is_flowing: bool,
    /// Source ID for event emission.
    source_id: Option<SourceId>,
}

impl FlowMonitor {
    fn new(source_id: Option<SourceId>) -> Self {
        Self {
            last_audio_time: Instant::now(),
            is_flowing: true, // Assume flowing initially
            source_id,
        }
    }

    /// Updates flow state based on whether samples contain non-zero audio.
    /// Returns an event to emit if state changed, or None.
    fn update(&mut self, has_audio: bool) -> Option<StreamEvent> {
        if has_audio {
            let was_stopped = !self.is_flowing;
            self.last_audio_time = Instant::now();
            self.is_flowing = true;

            if was_stopped {
                // Audio resumed after being stopped
                return Some(StreamEvent::AudioFlowResumed {
                    source_id: self
                        .source_id
                        .clone()
                        .unwrap_or_else(|| SourceId::new("default")),
                });
            }
        } else {
            let silence_duration = self.last_audio_time.elapsed();
            if self.is_flowing && silence_duration > SILENCE_THRESHOLD {
                self.is_flowing = false;
                return Some(StreamEvent::AudioFlowStopped {
                    source_id: self
                        .source_id
                        .clone()
                        .unwrap_or_else(|| SourceId::new("default")),
                    silent_duration_ms: silence_duration.as_millis() as u64,
                });
            }
        }
        None
    }
}

/// Configuration for the capture bridge task.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Device-native sample rate
    pub device_sample_rate: u32,
    /// Device-native channel count
    pub device_channels: u16,
    /// Target sample rate for output
    pub target_sample_rate: u32,
    /// Target channel count for output
    pub target_channels: u16,
    /// Duration of each chunk
    pub chunk_duration: Duration,
    /// Source identifier for multi-source capture (None for single-source).
    pub source_id: Option<SourceId>,
    /// Session start time for synchronized timestamps across sources.
    pub session_start: Instant,
}

/// The capture bridge reads audio from the ring buffer and forwards converted chunks.
///
/// This struct encapsulates the logic for:
/// 1. Polling the ring buffer at regular intervals
/// 2. Converting audio format (channels + sample rate)
/// 3. Tracking timestamps
/// 4. Forwarding to the router channel
/// 5. Monitoring audio flow and emitting events when audio stops/resumes
pub struct CaptureBridge {
    audio_buffer: AudioBuffer,
    converter: FormatConverter,
    chunk_tx: mpsc::Sender<AudioChunk>,
    state: Arc<SessionState>,
    target_sample_rate: u32,
    target_channels: u16,
    poll_interval: Duration,
    /// Source identifier for multi-source capture.
    source_id: Option<SourceId>,
    /// Monitors audio flow for stop/resume detection.
    flow_monitor: FlowMonitor,
    /// Optional callback for emitting flow events.
    event_callback: Option<EventCallback>,
    /// Session start time for synchronized timestamps across sources.
    session_start: Instant,
}

impl CaptureBridge {
    /// Creates a new capture bridge.
    pub fn new(
        ring_consumer: ringbuf::HeapCons<i16>,
        config: &CaptureConfig,
        chunk_tx: mpsc::Sender<AudioChunk>,
        state: Arc<SessionState>,
        event_callback: Option<EventCallback>,
    ) -> Self {
        tracing::info!(
            "CaptureBridge creating: source={:?}, device={}Hz/{}ch, target={}Hz/{}ch, chunk={:?}",
            config.source_id,
            config.device_sample_rate,
            config.device_channels,
            config.target_sample_rate,
            config.target_channels,
            config.chunk_duration
        );

        let audio_buffer = AudioBuffer::new(
            ring_consumer,
            config.device_sample_rate,
            config.device_channels,
            config.chunk_duration,
        );

        let converter = FormatConverter::new(
            config.device_sample_rate,
            config.device_channels,
            config.target_sample_rate,
            config.target_channels,
        );

        // Poll at half the chunk duration for responsiveness
        let poll_interval = config.chunk_duration / 2;

        Self {
            audio_buffer,
            converter,
            chunk_tx,
            state,
            target_sample_rate: config.target_sample_rate,
            target_channels: config.target_channels,
            poll_interval,
            source_id: config.source_id.clone(),
            flow_monitor: FlowMonitor::new(config.source_id.clone()),
            event_callback,
            session_start: config.session_start,
        }
    }

    /// Runs the capture bridge until stopped.
    ///
    /// This is the main loop that:
    /// 1. Polls for available audio at regular intervals
    /// 2. Converts and forwards complete chunks
    /// 3. Drains remaining audio on shutdown
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.poll_interval);
        // Start timestamp at current offset from session start for cross-source alignment
        let mut output_timestamp = self.session_start.elapsed();

        // Main capture loop
        while self.state.running.load(Ordering::SeqCst) {
            interval.tick().await;

            while let Some(chunk) = self.process_next_chunk(&mut output_timestamp) {
                if self.chunk_tx.send(chunk).await.is_err() {
                    // Router channel closed, stop capturing
                    return;
                }
            }
        }

        // Drain remaining audio on shutdown
        self.drain_remaining(&mut output_timestamp).await;
    }

    /// Processes the next available chunk from the ring buffer.
    fn process_next_chunk(&mut self, output_timestamp: &mut Duration) -> Option<AudioChunk> {
        if !self.audio_buffer.has_chunk() {
            // Even if no chunk is ready, check for silence timeout
            self.check_flow_timeout();
            return None;
        }

        let device_chunk = self.audio_buffer.try_read_chunk()?;
        let chunk = self.convert_chunk(&device_chunk, output_timestamp);

        // Check if this chunk has non-zero audio (not silence)
        let has_audio = chunk.samples.iter().any(|&s| s != 0);
        if let Some(event) = self.flow_monitor.update(has_audio) {
            self.emit_event(event);
        }

        // Log chunk production periodically
        let chunks = self.state.chunks_processed.load(Ordering::SeqCst);
        if chunks % 50 == 0 {
            tracing::debug!(
                "CaptureBridge {:?}: produced chunk #{}, {} samples, ts={:?}",
                self.source_id,
                chunks,
                chunk.samples.len(),
                chunk.timestamp
            );
        }

        // Update statistics
        self.state
            .samples_captured
            .fetch_add(chunk.samples.len() as u64, Ordering::SeqCst);
        self.state.chunks_processed.fetch_add(1, Ordering::SeqCst);

        Some(chunk)
    }

    /// Checks for silence timeout when no chunks are available.
    fn check_flow_timeout(&mut self) {
        // Update with has_audio=false to check if we've timed out
        if let Some(event) = self.flow_monitor.update(false) {
            self.emit_event(event);
        }
    }

    /// Emits an event via the callback if registered.
    fn emit_event(&self, event: StreamEvent) {
        if let Some(ref callback) = self.event_callback {
            callback(event);
        }
    }

    /// Converts a device-format chunk to target format.
    fn convert_chunk(
        &self,
        device_chunk: &AudioChunk,
        output_timestamp: &mut Duration,
    ) -> AudioChunk {
        let converted_samples = self.converter.convert(&device_chunk.samples);

        let chunk = if let Some(ref source_id) = self.source_id {
            AudioChunk::with_source(
                converted_samples,
                *output_timestamp,
                self.target_sample_rate,
                self.target_channels,
                source_id.clone(),
            )
        } else {
            AudioChunk::new(
                converted_samples,
                *output_timestamp,
                self.target_sample_rate,
                self.target_channels,
            )
        };

        *output_timestamp += chunk.duration();
        chunk
    }

    /// Drains all remaining audio from the buffer.
    async fn drain_remaining(&mut self, output_timestamp: &mut Duration) {
        for device_chunk in self.audio_buffer.drain() {
            let chunk = self.convert_chunk(&device_chunk, output_timestamp);
            // Best effort - don't block on send during shutdown
            let _ = self.chunk_tx.send(chunk).await;
        }
    }
}

/// Spawns the capture bridge as a background task.
pub fn spawn_capture_bridge(
    ring_consumer: ringbuf::HeapCons<i16>,
    config: &CaptureConfig,
    chunk_tx: mpsc::Sender<AudioChunk>,
    state: Arc<SessionState>,
    event_callback: Option<EventCallback>,
) -> tokio::task::JoinHandle<()> {
    let bridge = CaptureBridge::new(ring_consumer, config, chunk_tx, state, event_callback);
    tokio::spawn(bridge.run())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_config_creation() {
        let config = CaptureConfig {
            device_sample_rate: 48000,
            device_channels: 2,
            target_sample_rate: 16000,
            target_channels: 1,
            chunk_duration: Duration::from_millis(100),
            source_id: None,
            session_start: Instant::now(),
        };

        assert_eq!(config.device_sample_rate, 48000);
        assert_eq!(config.target_sample_rate, 16000);
    }

    #[test]
    fn test_capture_config_with_source_id() {
        let config = CaptureConfig {
            device_sample_rate: 48000,
            device_channels: 2,
            target_sample_rate: 16000,
            target_channels: 1,
            chunk_duration: Duration::from_millis(100),
            source_id: Some(SourceId::new("mic")),
            session_start: Instant::now(),
        };

        assert_eq!(config.source_id.as_ref().unwrap().as_str(), "mic");
    }
}
