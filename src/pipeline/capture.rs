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

/// Minimum gap duration to report as suspicious (50ms catches audible gaps).
const GAP_THRESHOLD_MS: u64 = 50;

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

/// Monitors for suspicious zero-sample gaps that indicate device issues.
///
/// Unlike `FlowMonitor` which detects when audio stops completely, `GapMonitor`
/// detects shorter gaps (50ms+) within the audio stream that indicate:
/// - Device contention (multiple apps using same audio device)
/// - USB bandwidth issues or buffer underruns
/// - Driver or OS audio subsystem problems
struct GapMonitor {
    /// Source ID for event emission.
    source_id: Option<SourceId>,
    /// Sample rate for duration calculations.
    sample_rate: u32,
    /// Threshold in samples (calculated from `GAP_THRESHOLD_MS` and `sample_rate`).
    threshold_samples: u32,
    /// Consecutive zero samples from previous chunk (for cross-chunk gaps).
    pending_zeros: u32,
    /// Position in samples where the pending gap started.
    pending_gap_start_samples: u64,
    /// Cumulative count of gaps detected.
    cumulative_gaps: u32,
    /// Cumulative gap duration in milliseconds.
    cumulative_gap_ms: u64,
}

impl GapMonitor {
    fn new(source_id: Option<SourceId>, sample_rate: u32) -> Self {
        let threshold_samples = (sample_rate as u64 * GAP_THRESHOLD_MS / 1000) as u32;
        Self {
            source_id,
            sample_rate,
            threshold_samples,
            pending_zeros: 0,
            pending_gap_start_samples: 0,
            cumulative_gaps: 0,
            cumulative_gap_ms: 0,
        }
    }

    /// Scans a chunk for zero gaps and returns events for any gaps found.
    ///
    /// Handles gaps that span chunk boundaries by tracking pending zeros.
    fn scan_chunk(&mut self, samples: &[i16], chunk_start_samples: u64) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let mut consecutive_zeros: u32 = self.pending_zeros;
        let mut gap_start_samples = if self.pending_zeros > 0 {
            self.pending_gap_start_samples
        } else {
            chunk_start_samples
        };

        for (i, &sample) in samples.iter().enumerate() {
            if sample == 0 {
                if consecutive_zeros == 0 {
                    gap_start_samples = chunk_start_samples + i as u64;
                }
                consecutive_zeros += 1;
            } else {
                // Non-zero sample: check if we had a gap
                if consecutive_zeros >= self.threshold_samples {
                    events.push(self.emit_gap_event(consecutive_zeros, gap_start_samples));
                }
                consecutive_zeros = 0;
            }
        }

        // Track any trailing zeros for the next chunk
        self.pending_zeros = consecutive_zeros;
        self.pending_gap_start_samples = gap_start_samples;

        events
    }

    /// Flushes any pending gap at end of stream.
    fn flush(&mut self) -> Option<StreamEvent> {
        if self.pending_zeros >= self.threshold_samples {
            let event = self.emit_gap_event(self.pending_zeros, self.pending_gap_start_samples);
            self.pending_zeros = 0;
            return Some(event);
        }
        None
    }

    fn emit_gap_event(&mut self, gap_samples: u32, gap_start_samples: u64) -> StreamEvent {
        let gap_duration_ms = (gap_samples as u64 * 1000) / self.sample_rate as u64;

        self.cumulative_gaps += 1;
        self.cumulative_gap_ms += gap_duration_ms;

        let position = Duration::from_secs_f64(gap_start_samples as f64 / self.sample_rate as f64);

        tracing::warn!(
            source = ?self.source_id,
            gap_ms = gap_duration_ms,
            gap_samples,
            position_s = position.as_secs_f64(),
            total_gaps = self.cumulative_gaps,
            "Audio gap detected - possible device contention or USB issue"
        );

        StreamEvent::AudioGapDetected {
            source_id: self
                .source_id
                .clone()
                .unwrap_or_else(|| SourceId::new("default")),
            gap_duration_ms,
            gap_samples,
            position,
            cumulative_gaps: self.cumulative_gaps,
            cumulative_gap_ms: self.cumulative_gap_ms,
        }
    }

    /// Returns the current cumulative stats.
    fn stats(&self) -> (u32, u64) {
        (self.cumulative_gaps, self.cumulative_gap_ms)
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
    /// Monitors for suspicious zero-sample gaps.
    gap_monitor: GapMonitor,
    /// Optional callback for emitting flow events.
    event_callback: Option<EventCallback>,
    /// Session start time for synchronized timestamps across sources.
    session_start: Instant,
    /// Running total of samples processed (for gap position tracking).
    samples_processed: u64,
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
            gap_monitor: GapMonitor::new(config.source_id.clone(), config.target_sample_rate),
            event_callback,
            session_start: config.session_start,
            samples_processed: 0,
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

        // Scan for suspicious zero gaps (device contention, USB issues)
        let gap_events = self
            .gap_monitor
            .scan_chunk(&chunk.samples, self.samples_processed);
        for event in gap_events {
            // Update session stats
            let (gaps, gap_ms) = self.gap_monitor.stats();
            self.state
                .audio_gaps_detected
                .store(gaps as u64, Ordering::SeqCst);
            self.state
                .total_gap_duration_ms
                .store(gap_ms, Ordering::SeqCst);
            self.emit_event(event);
        }
        self.samples_processed += chunk.samples.len() as u64;

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

            // Scan remaining chunks for gaps
            let gap_events = self
                .gap_monitor
                .scan_chunk(&chunk.samples, self.samples_processed);
            for event in gap_events {
                let (gaps, gap_ms) = self.gap_monitor.stats();
                self.state
                    .audio_gaps_detected
                    .store(gaps as u64, Ordering::SeqCst);
                self.state
                    .total_gap_duration_ms
                    .store(gap_ms, Ordering::SeqCst);
                self.emit_event(event);
            }
            self.samples_processed += chunk.samples.len() as u64;

            // Best effort - don't block on send during shutdown
            let _ = self.chunk_tx.send(chunk).await;
        }

        // Flush any pending gap at end of stream
        if let Some(event) = self.gap_monitor.flush() {
            let (gaps, gap_ms) = self.gap_monitor.stats();
            self.state
                .audio_gaps_detected
                .store(gaps as u64, Ordering::SeqCst);
            self.state
                .total_gap_duration_ms
                .store(gap_ms, Ordering::SeqCst);
            self.emit_event(event);
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
