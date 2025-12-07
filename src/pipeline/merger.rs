//! Time-window based audio merger for multi-source capture.
//!
//! The merger aligns audio chunks from multiple sources by timestamp,
//! grouping them into windows and combining samples when all sources
//! have contributed to a window.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::source::SourceId;
use crate::AudioChunk;

/// Merges audio chunks from multiple sources by timestamp window.
///
/// Chunks are assigned to time windows based on their timestamp. When all
/// expected sources have contributed to a window (or the timeout expires),
/// the merged chunk is emitted.
///
/// # Example
///
/// ```ignore
/// let mut merger = TimeWindowMerger::new(
///     Duration::from_millis(100),  // window duration
///     Duration::from_millis(200),  // timeout
///     vec!["mic".into(), "speaker".into()],
/// );
///
/// // Add chunks from sources
/// if let Some(merged) = merger.add_chunk(mic_chunk) {
///     // Merged chunk ready
/// }
/// ```
pub struct TimeWindowMerger {
    /// Duration of each merge window.
    window_duration: Duration,
    /// Timeout for incomplete windows.
    window_timeout: Duration,
    /// Expected sources for merge.
    expected_sources: Vec<SourceId>,
    /// Pending windows awaiting completion.
    pending: HashMap<u64, PendingWindow>,
    /// Output sample rate (for silence generation).
    sample_rate: u32,
    /// Output channel count (for silence generation).
    channels: u16,
    /// Maximum pending windows before oldest is evicted.
    max_pending: usize,
}

/// A window awaiting completion.
struct PendingWindow {
    /// Chunks from each source (cloned from input, sharing Arc samples).
    chunks: HashMap<SourceId, AudioChunk>,
    /// When this window was created.
    created_at: Instant,
    /// The timestamp of this window.
    timestamp: Duration,
}

impl TimeWindowMerger {

    /// Creates a new merger with a custom max pending window limit.
    pub fn with_max_pending(
        window_duration: Duration,
        window_timeout: Duration,
        expected_sources: Vec<SourceId>,
        sample_rate: u32,
        channels: u16,
        max_pending: usize,
    ) -> Self {
        Self {
            window_duration,
            window_timeout,
            expected_sources,
            pending: HashMap::new(),
            sample_rate,
            channels,
            max_pending,
        }
    }

    /// Adds a chunk to the merger.
    ///
    /// Returns merge results for any completed or evicted windows.
    /// Empty vec if still waiting for other sources.
    ///
    /// The chunk is cloned internally (cheap due to Arc-wrapped samples).
    pub fn add_chunk(&mut self, chunk: &AudioChunk) -> Vec<MergeResult> {
        let Some(source_id) = chunk.source_id.as_ref() else {
            return Vec::new();
        };
        let window_id = self.window_id_for_timestamp(chunk.timestamp);

        let window = self
            .pending
            .entry(window_id)
            .or_insert_with(|| PendingWindow {
                chunks: HashMap::new(),
                created_at: Instant::now(),
                timestamp: Duration::from_millis(
                    window_id * self.window_duration.as_millis() as u64,
                ),
            });

        window.chunks.insert(source_id.clone(), chunk.clone());

        let mut results = Vec::new();

        // Check if window is complete
        if self.is_window_complete(window_id) {
            if let Some(result) = self.emit_window(window_id) {
                results.push(result);
            }
        }

        // Evict oldest windows if over limit
        results.extend(self.evict_excess_windows());

        results
    }

    /// Evicts oldest windows if pending count exceeds `max_pending`.
    fn evict_excess_windows(&mut self) -> Vec<MergeResult> {
        let mut results = Vec::new();

        while self.pending.len() > self.max_pending {
            // Find oldest window by ID (lower ID = older timestamp)
            if let Some(&oldest_id) = self.pending.keys().min() {
                if let Some(result) = self.emit_window(oldest_id) {
                    results.push(result);
                }
            } else {
                break;
            }
        }

        results
    }

    /// Checks for timed-out windows and returns any that need emitting.
    ///
    /// Call this periodically to handle windows where some sources are missing.
    pub fn check_timeouts(&mut self) -> Vec<MergeResult> {
        let now = Instant::now();
        let timed_out: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, window)| now.duration_since(window.created_at) >= self.window_timeout)
            .map(|(&id, _)| id)
            .collect();

        timed_out
            .into_iter()
            .filter_map(|id| self.emit_window(id))
            .collect()
    }

    /// Returns the window ID for a given timestamp.
    fn window_id_for_timestamp(&self, timestamp: Duration) -> u64 {
        timestamp.as_millis() as u64 / self.window_duration.as_millis() as u64
    }

    /// Checks if a window has all expected sources.
    fn is_window_complete(&self, window_id: u64) -> bool {
        self.pending.get(&window_id).is_some_and(|window| {
            self.expected_sources
                .iter()
                .all(|id| window.chunks.contains_key(id))
        })
    }

    /// Emits a window (complete or timed out).
    fn emit_window(&mut self, window_id: u64) -> Option<MergeResult> {
        let window = self.pending.remove(&window_id)?;

        // Find missing sources
        let missing: Vec<SourceId> = self
            .expected_sources
            .iter()
            .filter(|id| !window.chunks.contains_key(*id))
            .cloned()
            .collect();

        // Merge available chunks
        let merged_chunk = self.merge_chunks(&window.chunks, window.timestamp);

        Some(MergeResult {
            chunk: merged_chunk,
            window_id,
            missing,
        })
    }

    /// Merges chunks from multiple sources into one using averaging.
    ///
    /// # Merge Strategy
    ///
    /// We use averaging (not summing) because:
    /// - No clipping risk: combined waveform stays within [-1, 1] range
    /// - ASR-friendly: clean, undistorted signal for speech recognition
    ///
    /// Trade-offs:
    /// - Volume drops when a source is missing (silence counts toward average)
    /// - Quiet sources become quieter when mixed with many sources
    ///
    /// If summing with clipping or normalization is needed, consider adding a
    /// `MergeMode` enum to make this configurable.
    fn merge_chunks(
        &self,
        chunks: &HashMap<SourceId, AudioChunk>,
        timestamp: Duration,
    ) -> AudioChunk {
        if chunks.is_empty() {
            // No chunks - return silence
            let samples_per_window = (self.sample_rate as f64 * self.window_duration.as_secs_f64())
                as usize
                * self.channels as usize;
            return AudioChunk::new(
                vec![0i16; samples_per_window],
                timestamp,
                self.sample_rate,
                self.channels,
            );
        }

        // Use first chunk to determine length
        let first = chunks.values().next().unwrap();
        let len = first.samples.len();

        // Accumulate samples in i32 to avoid overflow
        let mut merged = vec![0i32; len];
        let mut count = 0;

        for chunk in chunks.values() {
            // Handle different chunk lengths by using the minimum
            let chunk_len = chunk.samples.len().min(len);
            for (i, &sample) in chunk.samples.iter().take(chunk_len).enumerate() {
                merged[i] += sample as i32;
            }
            count += 1;
        }

        // Divide by total expected sources to maintain consistent volume.
        // Missing sources contribute silence (0), but we still count them in
        // the divisor to prevent volume spikes when sources drop.
        let total_sources = self.expected_sources.len();
        if count < total_sources {
            count = total_sources;
        }

        // Average and clamp to i16 range
        let samples: Vec<i16> = merged
            .iter()
            .map(|&s| {
                let avg = s / count as i32;
                avg.clamp(-32768, 32767) as i16
            })
            .collect();

        AudioChunk::from_arc(
            Arc::new(samples),
            timestamp,
            first.sample_rate,
            first.channels,
            None, // Merged chunks have no single source
        )
    }

    /// Returns the number of pending windows (test helper).
    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Clears all pending windows (test helper).
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// Result of a merge operation.
#[derive(Debug)]
pub struct MergeResult {
    /// The merged audio chunk.
    pub chunk: AudioChunk,
    /// The window ID that was merged.
    pub window_id: u64,
    /// Sources that were missing from this window (filled with silence).
    pub missing: Vec<SourceId>,
}

impl MergeResult {
    /// Returns true if this merge was complete (all sources present).
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Default max pending windows for tests (~1 second at 100ms windows).
    const DEFAULT_MAX_PENDING: usize = 10;

    impl TimeWindowMerger {
        /// Creates a merger with default max pending (test helper).
        fn new(
            window_duration: Duration,
            window_timeout: Duration,
            expected_sources: Vec<SourceId>,
            sample_rate: u32,
            channels: u16,
        ) -> Self {
            Self::with_max_pending(
                window_duration,
                window_timeout,
                expected_sources,
                sample_rate,
                channels,
                DEFAULT_MAX_PENDING,
            )
        }
    }

    /// Creates a test chunk with the given source, timestamp, and samples.
    fn make_chunk(source: &str, timestamp_ms: u64, samples: Vec<i16>) -> AudioChunk {
        AudioChunk::with_source(
            samples,
            Duration::from_millis(timestamp_ms),
            16000,
            1,
            SourceId::new(source),
        )
    }

    #[test]
    fn test_merger_complete_window() {
        let mut merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            vec![SourceId::new("mic"), SourceId::new("speaker")],
            16000,
            1,
        );

        // Add mic chunk for window 0 (0-100ms)
        let mic_chunk = make_chunk("mic", 50, vec![100, 200, 300]);
        let results = merger.add_chunk(&mic_chunk);
        assert!(results.is_empty()); // Not complete yet

        // Add speaker chunk for same window
        let speaker_chunk = make_chunk("speaker", 50, vec![50, 100, 150]);
        let results = merger.add_chunk(&speaker_chunk);
        assert_eq!(results.len(), 1); // Complete!

        let result = &results[0];
        assert!(result.is_complete());
        assert_eq!(result.window_id, 0);
        // Average of [100,200,300] and [50,100,150] = [75,150,225]
        assert_eq!(*result.chunk.samples, vec![75, 150, 225]);
    }

    #[test]
    fn test_merger_different_windows() {
        let mut merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            vec![SourceId::new("mic"), SourceId::new("speaker")],
            16000,
            1,
        );

        // Add chunks to different windows
        let mic_chunk = make_chunk("mic", 50, vec![100]); // Window 0
        let results = merger.add_chunk(&mic_chunk);
        assert!(results.is_empty());

        let speaker_chunk = make_chunk("speaker", 150, vec![200]); // Window 1
        let results = merger.add_chunk(&speaker_chunk);
        assert!(results.is_empty());

        // Both windows still pending
        assert_eq!(merger.pending_count(), 2);
    }

    #[test]
    fn test_merger_timeout() {
        let mut merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(0), // Immediate timeout
            vec![SourceId::new("mic"), SourceId::new("speaker")],
            16000,
            1,
        );

        // Add only mic chunk
        let mic_chunk = make_chunk("mic", 50, vec![100, 200]);
        merger.add_chunk(&mic_chunk);

        // Check timeouts - should emit incomplete window
        let results = merger.check_timeouts();
        assert_eq!(results.len(), 1);

        let result = &results[0];
        assert!(!result.is_complete());
        assert_eq!(result.missing.len(), 1);
        assert_eq!(result.missing[0], SourceId::new("speaker"));

        // With only 1 source of 2, average is: [100,200] / 2 = [50,100]
        assert_eq!(*result.chunk.samples, vec![50, 100]);
    }

    #[test]
    fn test_merger_window_id_calculation() {
        let merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            vec![SourceId::new("mic")],
            16000,
            1,
        );

        assert_eq!(merger.window_id_for_timestamp(Duration::from_millis(0)), 0);
        assert_eq!(merger.window_id_for_timestamp(Duration::from_millis(50)), 0);
        assert_eq!(merger.window_id_for_timestamp(Duration::from_millis(99)), 0);
        assert_eq!(
            merger.window_id_for_timestamp(Duration::from_millis(100)),
            1
        );
        assert_eq!(
            merger.window_id_for_timestamp(Duration::from_millis(250)),
            2
        );
    }

    #[test]
    fn test_merger_empty_chunks() {
        let merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            vec![SourceId::new("mic")],
            16000,
            1,
        );

        // merge_chunks with empty map
        let chunk = merger.merge_chunks(&HashMap::new(), Duration::ZERO);
        // Should be silence with expected sample count
        assert_eq!(chunk.samples.len(), 1600); // 100ms at 16kHz mono
        assert!(chunk.samples.iter().all(|&s| s == 0));
    }

    #[test]
    fn test_merger_clear() {
        let mut merger = TimeWindowMerger::new(
            Duration::from_millis(100),
            Duration::from_millis(200),
            vec![SourceId::new("mic"), SourceId::new("speaker")], // Two sources
            16000,
            1,
        );

        // Add only one source - window stays pending
        merger.add_chunk(&make_chunk("mic", 50, vec![100]));
        assert_eq!(merger.pending_count(), 1);

        merger.clear();
        assert_eq!(merger.pending_count(), 0);
    }

    #[test]
    fn test_merger_evicts_oldest_when_over_limit() {
        // Create merger with max 2 pending windows
        let mut merger = TimeWindowMerger::with_max_pending(
            Duration::from_millis(100),
            Duration::from_millis(1000), // Long timeout so we test eviction not timeout
            vec![SourceId::new("mic"), SourceId::new("speaker")],
            16000,
            1,
            2, // max 2 pending
        );

        // Add chunks to windows 0, 1 (from mic only - incomplete)
        merger.add_chunk(&make_chunk("mic", 50, vec![100])); // Window 0
        merger.add_chunk(&make_chunk("mic", 150, vec![200])); // Window 1
        assert_eq!(merger.pending_count(), 2);

        // Add chunk to window 2 - should evict window 0
        let results = merger.add_chunk(&make_chunk("mic", 250, vec![300])); // Window 2
        assert_eq!(results.len(), 1); // Window 0 evicted
        assert_eq!(results[0].window_id, 0);
        assert!(!results[0].is_complete()); // Missing speaker

        // Should now have windows 1 and 2
        assert_eq!(merger.pending_count(), 2);
    }
}
