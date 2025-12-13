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
                    source_id: self.source_id.clone().unwrap_or_default(),
                });
            }
        } else {
            let silence_duration = self.last_audio_time.elapsed();
            if self.is_flowing && silence_duration > SILENCE_THRESHOLD {
                self.is_flowing = false;
                return Some(StreamEvent::AudioFlowStopped {
                    source_id: self.source_id.clone().unwrap_or_default(),
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
            source_id: self.source_id.clone().unwrap_or_default(),
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

/// Effective silence floor for 16-bit audio in dB.
const SILENCE_FLOOR_DB: f32 = -96.0;

/// Calculates RMS level in dB relative to `i16::MAX`.
fn calculate_rms_db(sum_squares: f64, sample_count: usize) -> f32 {
    if sample_count == 0 {
        return SILENCE_FLOOR_DB;
    }
    let rms = (sum_squares / sample_count as f64).sqrt();
    if rms > 0.0 {
        20.0 * (rms / i16::MAX as f64).log10() as f32
    } else {
        SILENCE_FLOOR_DB
    }
}

/// Calculates DC offset as a ratio of `i16::MAX`.
fn calculate_dc_offset(sum: i64, sample_count: usize) -> f32 {
    if sample_count == 0 {
        return 0.0;
    }
    (sum as f64 / sample_count as f64 / i16::MAX as f64) as f32
}

/// Monitors audio quality metrics per-chunk (peak, RMS, clipping, DC offset).
///
/// Unlike `GapMonitor` which detects specific anomalies, `QualityMonitor` provides
/// continuous signal health metrics that downstream applications can use to:
/// - Warn about clipping (samples at max amplitude)
/// - Suggest gain adjustments
/// - Detect hardware issues via DC offset
struct QualityMonitor {
    /// Source ID for event emission.
    source_id: Option<SourceId>,
    /// Cumulative clipped samples across all chunks.
    total_clipped: u64,
}

impl QualityMonitor {
    fn new(source_id: Option<SourceId>) -> Self {
        Self {
            source_id,
            total_clipped: 0,
        }
    }

    /// Analyzes a chunk and returns a quality report event.
    ///
    /// Computes: peak amplitude, RMS level (dB), clipping count, DC offset.
    fn analyze_chunk(&mut self, samples: &[i16], position: Duration) -> StreamEvent {
        if samples.is_empty() {
            return self.empty_report(position);
        }

        let mut peak: i16 = 0;
        let mut sum_squares: f64 = 0.0;
        let mut sum: i64 = 0;
        let mut clipped: u32 = 0;

        for &s in samples {
            let abs = s.saturating_abs();
            if abs > peak {
                peak = abs;
            }
            sum_squares += (s as f64).powi(2);
            sum += s as i64;
            if s == i16::MAX || s == i16::MIN {
                clipped += 1;
            }
        }

        self.total_clipped += clipped as u64;

        StreamEvent::AudioQualityReport {
            source_id: self.source_id.clone().unwrap_or_default(),
            position,
            peak_amplitude: peak,
            rms_db: calculate_rms_db(sum_squares, samples.len()),
            clipped_samples: clipped,
            dc_offset: calculate_dc_offset(sum, samples.len()),
            total_clipped_samples: self.total_clipped,
        }
    }

    /// Returns a report for empty chunks (edge case).
    fn empty_report(&self, position: Duration) -> StreamEvent {
        StreamEvent::AudioQualityReport {
            source_id: self.source_id.clone().unwrap_or_default(),
            position,
            peak_amplitude: 0,
            rms_db: SILENCE_FLOOR_DB,
            clipped_samples: 0,
            dc_offset: 0.0,
            total_clipped_samples: self.total_clipped,
        }
    }

    /// Returns cumulative clipped sample count.
    fn total_clipped(&self) -> u64 {
        self.total_clipped
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
    /// Monitors audio quality metrics (peak, RMS, clipping, DC offset).
    quality_monitor: QualityMonitor,
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
            quality_monitor: QualityMonitor::new(config.source_id.clone()),
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
            self.check_flow_timeout();
            return None;
        }

        let device_chunk = self.audio_buffer.try_read_chunk()?;
        let chunk = self.convert_chunk(&device_chunk, output_timestamp);

        self.monitor_chunk(&chunk);
        self.update_session_stats(&chunk);

        Some(chunk)
    }

    /// Monitors a chunk for audio flow, gaps, and quality issues.
    fn monitor_chunk(&mut self, chunk: &AudioChunk) {
        let has_audio = chunk.samples.iter().any(|&s| s != 0);
        if let Some(event) = self.flow_monitor.update(has_audio) {
            self.emit_event(event);
        }

        let gap_events = self
            .gap_monitor
            .scan_chunk(&chunk.samples, self.samples_processed);
        for event in gap_events {
            self.update_gap_stats();
            self.emit_event(event);
        }

        let quality_event = self
            .quality_monitor
            .analyze_chunk(&chunk.samples, chunk.timestamp);
        self.state
            .clipped_samples
            .store(self.quality_monitor.total_clipped(), Ordering::SeqCst);
        self.emit_event(quality_event);

        self.samples_processed += chunk.samples.len() as u64;
    }

    /// Updates session statistics and logs progress periodically.
    fn update_session_stats(&self, chunk: &AudioChunk) {
        self.state
            .samples_captured
            .fetch_add(chunk.samples.len() as u64, Ordering::SeqCst);
        let chunks = self.state.chunks_processed.fetch_add(1, Ordering::SeqCst);

        if chunks % 50 == 0 {
            tracing::debug!(
                "CaptureBridge {:?}: produced chunk #{}, {} samples, ts={:?}",
                self.source_id,
                chunks,
                chunk.samples.len(),
                chunk.timestamp
            );
        }
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

    /// Updates session state with current gap statistics.
    fn update_gap_stats(&self) {
        let (gaps, gap_ms) = self.gap_monitor.stats();
        self.state
            .audio_gaps_detected
            .store(gaps as u64, Ordering::SeqCst);
        self.state
            .total_gap_duration_ms
            .store(gap_ms, Ordering::SeqCst);
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
                self.update_gap_stats();
                self.emit_event(event);
            }
            self.samples_processed += chunk.samples.len() as u64;

            // Best effort - don't block on send during shutdown
            let _ = self.chunk_tx.send(chunk).await;
        }

        // Flush any pending gap at end of stream
        if let Some(event) = self.gap_monitor.flush() {
            self.update_gap_stats();
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
    use ringbuf::traits::{Producer, Split};
    use ringbuf::HeapRb;
    use std::sync::Mutex;

    /// Helper to create a ring buffer pair for testing.
    fn create_test_ring_buffer(
        capacity: usize,
    ) -> (ringbuf::HeapProd<i16>, ringbuf::HeapCons<i16>) {
        let rb = HeapRb::<i16>::new(capacity);
        rb.split()
    }

    /// Helper to create a test CaptureConfig.
    fn create_test_config(
        device_sample_rate: u32,
        device_channels: u16,
        target_sample_rate: u32,
        target_channels: u16,
    ) -> CaptureConfig {
        CaptureConfig {
            device_sample_rate,
            device_channels,
            target_sample_rate,
            target_channels,
            chunk_duration: Duration::from_millis(100),
            source_id: Some(SourceId::new("test")),
            session_start: Instant::now(),
        }
    }

    /// Helper to capture events from the CaptureBridge.
    fn create_event_capture() -> (EventCallback, Arc<Mutex<Vec<StreamEvent>>>) {
        let events: Arc<Mutex<Vec<StreamEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = Arc::clone(&events);
        let callback: EventCallback = Arc::new(move |event| {
            events_clone.lock().unwrap().push(event);
        });
        (callback, events)
    }

    #[test]
    fn test_calculate_rms_db_normal() {
        // Full-scale sine wave has RMS of ~0.707, which is about -3dB
        let sum_squares = (i16::MAX as f64).powi(2) * 100.0; // 100 samples at max
        let rms_db = calculate_rms_db(sum_squares, 100);
        assert!((rms_db - 0.0).abs() < 0.1); // Should be ~0dB for full scale
    }

    #[test]
    fn test_calculate_rms_db_silence() {
        let rms_db = calculate_rms_db(0.0, 100);
        assert_eq!(rms_db, SILENCE_FLOOR_DB);
    }

    #[test]
    fn test_calculate_rms_db_empty() {
        let rms_db = calculate_rms_db(0.0, 0);
        assert_eq!(rms_db, SILENCE_FLOOR_DB);
    }

    #[test]
    fn test_calculate_dc_offset_zero() {
        let dc = calculate_dc_offset(0, 100);
        assert_eq!(dc, 0.0);
    }

    #[test]
    fn test_calculate_dc_offset_positive() {
        // All samples at half max = 50% DC offset
        let sum = (i16::MAX as i64 / 2) * 100;
        let dc = calculate_dc_offset(sum, 100);
        assert!((dc - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_calculate_dc_offset_empty() {
        let dc = calculate_dc_offset(1000, 0);
        assert_eq!(dc, 0.0);
    }

    // ==================== FlowMonitor Tests ====================

    #[test]
    fn test_flow_monitor_initial_state_is_flowing() {
        let monitor = FlowMonitor::new(Some(SourceId::new("test")));
        assert!(monitor.is_flowing);
    }

    #[test]
    fn test_flow_monitor_audio_keeps_flowing_no_event() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Continuous audio should not emit any events
        assert!(monitor.update(true).is_none());
        assert!(monitor.update(true).is_none());
        assert!(monitor.update(true).is_none());
        assert!(monitor.is_flowing);
    }

    #[test]
    fn test_flow_monitor_silence_under_threshold_no_event() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Start with audio
        assert!(monitor.update(true).is_none());
        // Brief silence under 500ms threshold should not emit
        std::thread::sleep(Duration::from_millis(100));
        assert!(monitor.update(false).is_none());
        assert!(monitor.is_flowing); // Still considered flowing
    }

    #[test]
    fn test_flow_monitor_silence_over_threshold_emits_stopped() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Start with audio
        assert!(monitor.update(true).is_none());
        // Wait past silence threshold
        std::thread::sleep(Duration::from_millis(510));
        let event = monitor.update(false);
        assert!(event.is_some());
        if let Some(StreamEvent::AudioFlowStopped {
            source_id,
            silent_duration_ms,
        }) = event
        {
            assert_eq!(source_id.as_str(), "test");
            assert!(silent_duration_ms >= 500);
        } else {
            panic!("Expected AudioFlowStopped event");
        }
        assert!(!monitor.is_flowing);
    }

    #[test]
    fn test_flow_monitor_audio_resumes_after_stopped_emits_resumed() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Get into stopped state
        std::thread::sleep(Duration::from_millis(510));
        let _ = monitor.update(false);
        assert!(!monitor.is_flowing);

        // Resume with audio
        let event = monitor.update(true);
        assert!(event.is_some());
        if let Some(StreamEvent::AudioFlowResumed { source_id }) = event {
            assert_eq!(source_id.as_str(), "test");
        } else {
            panic!("Expected AudioFlowResumed event");
        }
        assert!(monitor.is_flowing);
    }

    #[test]
    fn test_flow_monitor_resumed_only_emits_once() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Get into stopped state
        std::thread::sleep(Duration::from_millis(510));
        let _ = monitor.update(false);

        // First audio should emit resumed
        assert!(monitor.update(true).is_some());
        // Subsequent audio should not emit again
        assert!(monitor.update(true).is_none());
        assert!(monitor.update(true).is_none());
    }

    #[test]
    fn test_flow_monitor_stopped_only_emits_once() {
        let mut monitor = FlowMonitor::new(Some(SourceId::new("test")));
        // Wait past threshold and emit stopped
        std::thread::sleep(Duration::from_millis(510));
        assert!(monitor.update(false).is_some());
        // Further silence should not emit again
        std::thread::sleep(Duration::from_millis(100));
        assert!(monitor.update(false).is_none());
    }

    #[test]
    fn test_flow_monitor_without_source_id_uses_default() {
        let mut monitor = FlowMonitor::new(None);
        std::thread::sleep(Duration::from_millis(510));
        let event = monitor.update(false);
        if let Some(StreamEvent::AudioFlowStopped { source_id, .. }) = event {
            assert_eq!(source_id.as_str(), "default");
        } else {
            panic!("Expected AudioFlowStopped event");
        }
    }

    // ==================== GapMonitor Tests ====================

    #[test]
    fn test_gap_monitor_no_gap_when_nonzero_samples() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        let samples: Vec<i16> = vec![100; 1000];
        let events = monitor.scan_chunk(&samples, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn test_gap_monitor_short_gap_under_threshold_no_event() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // At 16kHz, 50ms = 800 samples. Create a 400-sample gap (25ms).
        let mut samples = vec![100i16; 100];
        samples.extend(vec![0i16; 400]); // Under threshold
        samples.extend(vec![100i16; 100]);

        let events = monitor.scan_chunk(&samples, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn test_gap_monitor_gap_at_threshold_emits_event() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // At 16kHz, 50ms = 800 samples
        let mut samples = vec![100i16; 100];
        samples.extend(vec![0i16; 800]); // Exactly at threshold
        samples.extend(vec![100i16; 100]);

        let events = monitor.scan_chunk(&samples, 0);
        assert_eq!(events.len(), 1);
        if let StreamEvent::AudioGapDetected {
            gap_duration_ms,
            gap_samples,
            ..
        } = &events[0]
        {
            assert_eq!(*gap_samples, 800);
            assert_eq!(*gap_duration_ms, 50);
        } else {
            panic!("Expected AudioGapDetected event");
        }
    }

    #[test]
    fn test_gap_monitor_gap_over_threshold_emits_event() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // 1600 samples = 100ms at 16kHz
        let mut samples = vec![100i16; 100];
        samples.extend(vec![0i16; 1600]);
        samples.extend(vec![100i16; 100]);

        let events = monitor.scan_chunk(&samples, 0);
        assert_eq!(events.len(), 1);
        if let StreamEvent::AudioGapDetected {
            gap_duration_ms,
            gap_samples,
            ..
        } = &events[0]
        {
            assert_eq!(*gap_samples, 1600);
            assert_eq!(*gap_duration_ms, 100);
        } else {
            panic!("Expected AudioGapDetected event");
        }
    }

    #[test]
    fn test_gap_monitor_gap_spanning_chunks() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // First chunk ends with 400 zeros
        let mut chunk1 = vec![100i16; 100];
        chunk1.extend(vec![0i16; 400]);
        let events1 = monitor.scan_chunk(&chunk1, 0);
        assert!(events1.is_empty()); // Not enough yet

        // Second chunk starts with 500 zeros (total 900 > 800 threshold)
        let mut chunk2 = vec![0i16; 500];
        chunk2.extend(vec![100i16; 100]);
        let events2 = monitor.scan_chunk(&chunk2, 500);
        assert_eq!(events2.len(), 1);
        if let StreamEvent::AudioGapDetected { gap_samples, .. } = &events2[0] {
            assert_eq!(*gap_samples, 900); // 400 + 500
        } else {
            panic!("Expected AudioGapDetected event");
        }
    }

    #[test]
    fn test_gap_monitor_multiple_gaps_in_chunk() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // Two separate gaps in one chunk
        let mut samples = vec![100i16; 50];
        samples.extend(vec![0i16; 900]); // First gap
        samples.extend(vec![100i16; 50]);
        samples.extend(vec![0i16; 1000]); // Second gap
        samples.extend(vec![100i16; 50]);

        let events = monitor.scan_chunk(&samples, 0);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_gap_monitor_flush_emits_pending_gap() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // Chunk ending with zeros at threshold
        let mut samples = vec![100i16; 100];
        samples.extend(vec![0i16; 900]);
        let events = monitor.scan_chunk(&samples, 0);
        assert!(events.is_empty()); // Gap not closed yet

        // Flush should emit the pending gap
        let flush_event = monitor.flush();
        assert!(flush_event.is_some());
        if let Some(StreamEvent::AudioGapDetected { gap_samples, .. }) = flush_event {
            assert_eq!(gap_samples, 900);
        } else {
            panic!("Expected AudioGapDetected event");
        }
    }

    #[test]
    fn test_gap_monitor_flush_no_event_when_under_threshold() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // Chunk ending with zeros under threshold
        let mut samples = vec![100i16; 100];
        samples.extend(vec![0i16; 400]);
        let _ = monitor.scan_chunk(&samples, 0);

        // Flush should not emit (under threshold)
        assert!(monitor.flush().is_none());
    }

    #[test]
    fn test_gap_monitor_cumulative_stats() {
        let mut monitor = GapMonitor::new(Some(SourceId::new("test")), 16000);
        // Create multiple gaps
        let mut samples = vec![100i16; 50];
        samples.extend(vec![0i16; 800]); // 50ms gap
        samples.extend(vec![100i16; 50]);
        samples.extend(vec![0i16; 1600]); // 100ms gap
        samples.extend(vec![100i16; 50]);

        let _ = monitor.scan_chunk(&samples, 0);
        let (gaps, total_ms) = monitor.stats();
        assert_eq!(gaps, 2);
        assert_eq!(total_ms, 150); // 50 + 100
    }

    #[test]
    fn test_gap_monitor_threshold_calculation_44100hz() {
        let monitor = GapMonitor::new(None, 44100);
        // 50ms at 44100Hz = 2205 samples
        assert_eq!(monitor.threshold_samples, 2205);
    }

    #[test]
    fn test_gap_monitor_threshold_calculation_16000hz() {
        let monitor = GapMonitor::new(None, 16000);
        // 50ms at 16000Hz = 800 samples
        assert_eq!(monitor.threshold_samples, 800);
    }

    // ==================== QualityMonitor Tests ====================

    #[test]
    fn test_quality_monitor_peak_amplitude_positive() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![100i16, 200, 300, 150];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { peak_amplitude, .. } = event {
            assert_eq!(peak_amplitude, 300);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_peak_amplitude_negative() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![-100i16, -500, -200];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { peak_amplitude, .. } = event {
            assert_eq!(peak_amplitude, 500); // Absolute value
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_peak_amplitude_mixed() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![100i16, -600, 300, -200];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { peak_amplitude, .. } = event {
            assert_eq!(peak_amplitude, 600);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_rms_silence() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![0i16; 100];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { rms_db, .. } = event {
            assert_eq!(rms_db, SILENCE_FLOOR_DB);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_rms_full_scale() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        // All samples at max should give ~0dB
        let samples = vec![i16::MAX; 100];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { rms_db, .. } = event {
            assert!((rms_db - 0.0).abs() < 0.1);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_clipping_at_max() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![i16::MAX, i16::MAX, 100, 200];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport {
            clipped_samples,
            total_clipped_samples,
            ..
        } = event
        {
            assert_eq!(clipped_samples, 2);
            assert_eq!(total_clipped_samples, 2);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_clipping_at_min() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples = vec![i16::MIN, i16::MIN, i16::MIN, 100];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport {
            clipped_samples, ..
        } = event
        {
            assert_eq!(clipped_samples, 3);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_cumulative_clipping() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));

        // First chunk with 2 clipped
        let samples1 = vec![i16::MAX, i16::MAX, 100];
        let _ = monitor.analyze_chunk(&samples1, Duration::ZERO);

        // Second chunk with 3 clipped
        let samples2 = vec![i16::MIN, i16::MIN, i16::MIN];
        let event = monitor.analyze_chunk(&samples2, Duration::from_millis(100));

        if let StreamEvent::AudioQualityReport {
            clipped_samples,
            total_clipped_samples,
            ..
        } = event
        {
            assert_eq!(clipped_samples, 3); // This chunk
            assert_eq!(total_clipped_samples, 5); // Cumulative
        } else {
            panic!("Expected AudioQualityReport");
        }
        assert_eq!(monitor.total_clipped(), 5);
    }

    #[test]
    fn test_quality_monitor_dc_offset_zero_for_centered() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        // Balanced positive and negative
        let samples = vec![1000i16, -1000, 500, -500];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { dc_offset, .. } = event {
            assert!(dc_offset.abs() < 0.001);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_dc_offset_positive_bias() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        // All positive samples at half max
        let half_max = i16::MAX / 2;
        let samples = vec![half_max; 100];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { dc_offset, .. } = event {
            assert!((dc_offset - 0.5).abs() < 0.01);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_dc_offset_negative_bias() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let half_min = i16::MIN / 2;
        let samples = vec![half_min; 100];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { dc_offset, .. } = event {
            assert!((dc_offset - (-0.5)).abs() < 0.01);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_empty_chunk() {
        let mut monitor = QualityMonitor::new(Some(SourceId::new("test")));
        let samples: Vec<i16> = vec![];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport {
            peak_amplitude,
            rms_db,
            clipped_samples,
            dc_offset,
            ..
        } = event
        {
            assert_eq!(peak_amplitude, 0);
            assert_eq!(rms_db, SILENCE_FLOOR_DB);
            assert_eq!(clipped_samples, 0);
            assert_eq!(dc_offset, 0.0);
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    #[test]
    fn test_quality_monitor_without_source_id_uses_default() {
        let mut monitor = QualityMonitor::new(None);
        let samples = vec![100i16; 10];
        let event = monitor.analyze_chunk(&samples, Duration::ZERO);
        if let StreamEvent::AudioQualityReport { source_id, .. } = event {
            assert_eq!(source_id.as_str(), "default");
        } else {
            panic!("Expected AudioQualityReport");
        }
    }

    // ==================== CaptureBridge Async Tests ====================

    #[tokio::test]
    async fn test_capture_bridge_processes_single_chunk() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());
        let (callback, _events) = create_event_capture();

        // Push exactly one chunk worth of samples (100ms at 16kHz = 1600 samples)
        for i in 0..1600i16 {
            let _ = producer.try_push(i);
        }

        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), Some(callback));

        // Run bridge in background, stop quickly
        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        // Wait for processing
        tokio::time::sleep(Duration::from_millis(150)).await;
        state_clone.running.store(false, Ordering::SeqCst);

        handle.await.unwrap();

        // Should have received one chunk
        let chunk = rx.try_recv().expect("Should receive a chunk");
        assert_eq!(chunk.samples.len(), 1600);
        assert_eq!(chunk.sample_rate, 16000);
        assert_eq!(chunk.channels, 1);
    }

    #[tokio::test]
    async fn test_capture_bridge_processes_multiple_chunks() {
        let (mut producer, consumer) = create_test_ring_buffer(20000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 3 chunks worth of samples (4800 samples)
        for i in 0..4800i16 {
            let _ = producer.try_push(i % 1000);
        }

        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        // Should have received 3 chunks
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }
        assert_eq!(chunks.len(), 3);
    }

    #[tokio::test]
    async fn test_capture_bridge_applies_format_conversion() {
        let (mut producer, consumer) = create_test_ring_buffer(20000);
        // Device: 48kHz stereo -> Target: 16kHz mono
        let config = create_test_config(48000, 2, 16000, 1);
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 100ms at 48kHz stereo = 9600 samples (4800 frames * 2 channels)
        // Stereo pairs: L=500, R=300 -> mono average = 400
        for _ in 0..4800 {
            let _ = producer.try_push(500); // Left
            let _ = producer.try_push(300); // Right
        }

        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let chunk = rx.try_recv().expect("Should receive converted chunk");
        // 48kHz -> 16kHz = 3:1, so 4800 frames -> 1600 frames
        // mono = 1 channel, so 1600 samples
        assert_eq!(chunk.sample_rate, 16000);
        assert_eq!(chunk.channels, 1);
        assert_eq!(chunk.samples.len(), 1600);
        // Verify conversion: stereo (500, 300) -> mono 400
        assert!(chunk.samples.iter().all(|&s| s == 400));
    }

    #[tokio::test]
    async fn test_capture_bridge_timestamps_monotonic() {
        let (mut producer, consumer) = create_test_ring_buffer(20000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 3 chunks worth
        for _ in 0..4800i16 {
            let _ = producer.try_push(100);
        }

        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let state_clone = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }

        // Verify timestamps are monotonically increasing
        for i in 1..chunks.len() {
            assert!(
                chunks[i].timestamp > chunks[i - 1].timestamp,
                "Timestamps should be monotonically increasing"
            );
        }

        // Each chunk is 100ms, so second chunk should be ~100ms after first
        if chunks.len() >= 2 {
            let diff = chunks[1].timestamp - chunks[0].timestamp;
            assert!(
                (diff.as_millis() as i64 - 100).abs() < 10,
                "Chunk spacing should be ~100ms"
            );
        }
    }

    #[tokio::test]
    async fn test_capture_bridge_updates_samples_captured() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 2 chunks worth (3200 samples)
        for _ in 0..3200i16 {
            let _ = producer.try_push(100);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        // Should have captured 3200 samples (2 chunks)
        let captured = state.samples_captured.load(Ordering::SeqCst);
        assert_eq!(captured, 3200, "Should track samples captured correctly");
    }

    #[tokio::test]
    async fn test_capture_bridge_updates_chunks_processed() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 2 chunks worth
        for _ in 0..3200i16 {
            let _ = producer.try_push(100);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let chunks = state.chunks_processed.load(Ordering::SeqCst);
        assert_eq!(chunks, 2, "Should track chunks processed correctly");
    }

    #[tokio::test]
    async fn test_capture_bridge_tracks_clipped_samples() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push chunk with clipped samples (i16::MAX and i16::MIN count as clipped)
        for i in 0..1600i16 {
            if i < 10 {
                let _ = producer.try_push(i16::MAX);
            } else if i < 20 {
                let _ = producer.try_push(i16::MIN);
            } else {
                let _ = producer.try_push(100);
            }
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let clipped = state.clipped_samples.load(Ordering::SeqCst);
        assert_eq!(
            clipped, 20,
            "Should track 20 clipped samples (10 MAX + 10 MIN)"
        );
    }

    #[tokio::test]
    async fn test_capture_bridge_stops_on_running_false() {
        let (mut producer, consumer) = create_test_ring_buffer(50000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(100);
        let state = Arc::new(SessionState::new());

        // Push a lot of data
        for _ in 0..32000i16 {
            let _ = producer.try_push(100);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        // Stop immediately
        state_clone.running.store(false, Ordering::SeqCst);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        // Should complete quickly since running is false
        let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
        assert!(result.is_ok(), "Bridge should stop when running is false");
    }

    #[tokio::test]
    async fn test_capture_bridge_drains_remaining_on_stop() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());

        // Push 2.5 chunks worth (4000 samples = 2 full + 800 partial)
        for i in 0..4000i16 {
            let _ = producer.try_push(i % 1000);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), None);

        // Stop immediately to trigger drain
        state_clone.running.store(false, Ordering::SeqCst);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        handle.await.unwrap();

        // Drain should have produced all chunks including partial
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }

        // Should have 3 chunks (2 full of 1600 + 1 partial of 800)
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].samples.len(), 1600);
        assert_eq!(chunks[1].samples.len(), 1600);
        assert_eq!(chunks[2].samples.len(), 800);
    }

    #[tokio::test]
    async fn test_capture_bridge_flushes_gaps_on_drain() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());
        let (callback, events) = create_event_capture();

        // Push chunk that ends with a long gap (over threshold)
        // 100 non-zero samples, then 900 zeros (56ms gap at 16kHz > 50ms threshold)
        for i in 0..100i16 {
            let _ = producer.try_push(i + 1);
        }
        for _ in 0..900i16 {
            let _ = producer.try_push(0);
        }
        // Add more to make a complete chunk
        for _ in 0..600i16 {
            let _ = producer.try_push(0);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), Some(callback));

        // Stop immediately to trigger drain+flush
        state_clone.running.store(false, Ordering::SeqCst);

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        handle.await.unwrap();

        // Check for gap event
        let captured_events = events.lock().unwrap();
        let gap_events: Vec<_> = captured_events
            .iter()
            .filter(|e| matches!(e, StreamEvent::AudioGapDetected { .. }))
            .collect();

        // The gap monitor should detect and flush the trailing gap
        // (1500 consecutive zeros > 800 threshold at 16kHz)
        assert!(
            !gap_events.is_empty(),
            "Should emit gap event for trailing zeros"
        );
    }

    #[tokio::test]
    async fn test_capture_bridge_emits_quality_events() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());
        let (callback, events) = create_event_capture();

        // Push one chunk with known characteristics
        for _ in 0..1600i16 {
            let _ = producer.try_push(500);
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), Some(callback));

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let captured_events = events.lock().unwrap();
        let quality_events: Vec<_> = captured_events
            .iter()
            .filter(|e| matches!(e, StreamEvent::AudioQualityReport { .. }))
            .collect();

        assert!(!quality_events.is_empty(), "Should emit quality events");
        if let StreamEvent::AudioQualityReport { peak_amplitude, .. } = quality_events[0] {
            assert_eq!(*peak_amplitude, 500, "Peak should match input");
        }
    }

    #[tokio::test]
    async fn test_capture_bridge_emits_gap_events() {
        let (mut producer, consumer) = create_test_ring_buffer(10000);
        let config = create_test_config(16000, 1, 16000, 1);
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let state = Arc::new(SessionState::new());
        let (callback, events) = create_event_capture();

        // Push chunk with a gap exceeding threshold (800 samples = 50ms at 16kHz)
        for i in 0..1600i16 {
            if i < 100 || i >= 1000 {
                let _ = producer.try_push(500);
            } else {
                let _ = producer.try_push(0); // 900 zeros = gap
            }
        }

        let state_clone = Arc::clone(&state);
        let bridge = CaptureBridge::new(consumer, &config, tx, Arc::clone(&state), Some(callback));

        let handle = tokio::spawn(async move {
            bridge.run().await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        state_clone.running.store(false, Ordering::SeqCst);
        handle.await.unwrap();

        let captured_events = events.lock().unwrap();
        let gap_events: Vec<_> = captured_events
            .iter()
            .filter(|e| matches!(e, StreamEvent::AudioGapDetected { .. }))
            .collect();

        assert!(
            !gap_events.is_empty(),
            "Should emit gap event for 900 zeros"
        );
        if let StreamEvent::AudioGapDetected { gap_samples, .. } = gap_events[0] {
            assert_eq!(*gap_samples, 900, "Gap should be 900 samples");
        }
    }
}
