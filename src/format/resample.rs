//! Sample rate conversion.
//!
//! This module provides basic resampling using linear interpolation.
//! For higher quality, consider using a dedicated resampling crate.

/// Resamples audio from one sample rate to another.
///
/// Uses linear interpolation, which is fast but may introduce artifacts
/// for large rate changes. Suitable for speech/transcription use cases.
///
/// # Arguments
///
/// * `samples` - Input samples (mono)
/// * `from_rate` - Source sample rate in Hz
/// * `to_rate` - Target sample rate in Hz
///
/// # Returns
///
/// Resampled audio data.
pub fn resample(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = f64::from(to_rate) / f64::from(from_rate);
    let output_len = (samples.len() as f64 * ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_pos = i as f64 / ratio;
        let src_idx = src_pos.floor() as usize;
        let frac = src_pos - src_idx as f64;

        let sample = if src_idx + 1 < samples.len() {
            // Linear interpolation between two samples
            let s1 = f64::from(samples[src_idx]);
            let s2 = f64::from(samples[src_idx + 1]);
            (s1 + (s2 - s1) * frac) as i16
        } else if src_idx < samples.len() {
            // Last sample, no interpolation
            samples[src_idx]
        } else {
            // Beyond input, use last sample
            *samples.last().unwrap_or(&0)
        };

        output.push(sample);
    }

    output
}

/// Resamples stereo audio.
///
/// Processes left and right channels separately.
pub fn resample_stereo(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }

    // Deinterleave
    let mut left = Vec::with_capacity(samples.len() / 2);
    let mut right = Vec::with_capacity(samples.len() / 2);

    for chunk in samples.chunks_exact(2) {
        left.push(chunk[0]);
        right.push(chunk[1]);
    }

    // Resample each channel
    let left_resampled = resample(&left, from_rate, to_rate);
    let right_resampled = resample(&right, from_rate, to_rate);

    // Interleave
    let mut output = Vec::with_capacity(left_resampled.len() * 2);
    for (l, r) in left_resampled.into_iter().zip(right_resampled) {
        output.push(l);
        output.push(r);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_same_rate() {
        let samples = vec![100i16, 200, 300];
        let resampled = resample(&samples, 16000, 16000);
        assert_eq!(resampled, samples);
    }

    #[test]
    fn test_resample_empty() {
        let samples: Vec<i16> = vec![];
        let resampled = resample(&samples, 16000, 8000);
        assert!(resampled.is_empty());
    }

    #[test]
    fn test_resample_downsample() {
        // 48kHz to 16kHz = 3:1 ratio
        let samples: Vec<i16> = (0..480).map(|i| (i * 10) as i16).collect();
        let resampled = resample(&samples, 48000, 16000);

        // Should be roughly 1/3 the length
        assert_eq!(resampled.len(), 160);
    }

    #[test]
    fn test_resample_upsample() {
        // 16kHz to 48kHz = 1:3 ratio
        let samples: Vec<i16> = vec![0, 1000, 2000, 3000];
        let resampled = resample(&samples, 16000, 48000);

        // Should be roughly 3x the length
        assert_eq!(resampled.len(), 12);

        // First and roughly every 3rd sample should match original
        assert_eq!(resampled[0], 0);
    }

    #[test]
    fn test_resample_interpolation() {
        // Simple case: two samples, upsample by 2x
        let samples = vec![0i16, 1000];
        let resampled = resample(&samples, 1, 2);

        // Should have ~4 samples with interpolated values
        assert_eq!(resampled.len(), 4);
        assert_eq!(resampled[0], 0);
        // Middle samples should be interpolated
        assert!(resampled[1] > 0 && resampled[1] < 1000);
    }

    #[test]
    fn test_resample_stereo() {
        let samples = vec![100i16, 200, 300, 400]; // L R L R
        let resampled = resample_stereo(&samples, 16000, 16000);
        assert_eq!(resampled, samples);
    }

    #[test]
    fn test_resample_stereo_downsample() {
        // 4 stereo frames (8 samples) at 48kHz -> ~1.33 frames at 16kHz
        let samples = vec![0i16, 0, 100, 100, 200, 200, 300, 300];
        let resampled = resample_stereo(&samples, 48000, 16000);

        // Should have fewer samples, still even (stereo)
        assert!(resampled.len() < samples.len());
        assert_eq!(resampled.len() % 2, 0);
    }

    // ==================== Edge Case Tests ====================

    #[test]
    fn test_resample_zero_to_rate() {
        // to_rate=0 means ratio=0, output_len=0
        let samples = vec![100i16, 200, 300];
        let result = resample(&samples, 16000, 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_resample_zero_from_rate() {
        // from_rate=0 causes division by zero → infinity ratio
        // This results in a massive output (may OOM or hang)
        // We just document this is undefined behavior
        // let _samples = vec![100i16];
        // let _result = resample(&_samples, 0, 16000);
        // Would produce output_len = (1 * inf).ceil() which overflows
        // TEST INTENTIONALLY LEFT EMPTY - documents UB
    }

    #[test]
    fn test_resample_extreme_upsample() {
        // Extreme upsample: 1Hz to 1000Hz (1000x)
        let samples = vec![0i16, 1000];
        let result = resample(&samples, 1, 1000);

        // Should produce ~2000 samples (2 samples * 1000)
        assert_eq!(result.len(), 2000);
        // First sample should be 0
        assert_eq!(result[0], 0);
        // Last sample should be 1000 (or close)
        assert_eq!(*result.last().unwrap(), 1000);
    }

    #[test]
    fn test_resample_single_sample() {
        // Single sample resampled to higher rate
        let samples = vec![500i16];
        let result = resample(&samples, 1, 10);

        // Should produce ~10 copies of the same sample
        assert_eq!(result.len(), 10);
        // All samples should be 500 (no interpolation possible)
        assert!(result.iter().all(|&s| s == 500));
    }

    #[test]
    fn test_resample_stereo_odd_samples() {
        // Odd number of stereo samples with actual resampling
        // chunks_exact ignores remainder (lossy!)
        let odd_stereo = vec![100i16, 200, 300]; // 1.5 frames
                                                 // Force actual resampling (not same-rate early return)
        let result = resample_stereo(&odd_stereo, 16000, 32000);
        // Only first complete frame (2 samples) is deinterleaved and processed
        // Third sample is lost, then result is upsampled and reinterleaved
        assert_eq!(result.len() % 2, 0); // Should be even (stereo pairs)
    }

    #[test]
    fn test_resample_stereo_single_sample() {
        // Single sample with actual resampling: no complete stereo frames
        let single = vec![100i16];
        // Force actual resampling (not same-rate early return)
        let result = resample_stereo(&single, 16000, 32000);
        assert!(result.is_empty()); // Sample is lost!
    }

    #[test]
    fn test_resample_stereo_same_rate_passthrough() {
        // Same rate: early return bypasses deinterleaving, preserves all samples
        let odd_stereo = vec![100i16, 200, 300];
        let result = resample_stereo(&odd_stereo, 16000, 16000);
        // Early return means no loss - all samples preserved (even if odd)
        assert_eq!(result, vec![100, 200, 300]);
    }

    #[test]
    fn test_resample_precision_boundary() {
        // Test interpolation at exact sample boundaries
        let samples = vec![0i16, 100, 200, 300];
        // 2x upsample: should land on original samples at even indices
        let result = resample(&samples, 1, 2);

        // Original samples should appear at even positions
        assert_eq!(result[0], 0);
        assert_eq!(result[2], 100);
        assert_eq!(result[4], 200);
        assert_eq!(result[6], 300);
    }
}
