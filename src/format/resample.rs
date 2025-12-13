//! Sample rate conversion.
//!
//! This module provides basic resampling using linear interpolation.
//! For higher quality, consider using a dedicated resampling crate.

use super::FormatError;

/// Resamples audio from one sample rate to another (checked version).
///
/// Uses linear interpolation, which is fast but may introduce artifacts
/// for large rate changes. Suitable for speech/transcription use cases.
///
/// Returns `Err(FormatError::ZeroSampleRate)` if `from_rate` is zero.
///
/// # Arguments
///
/// * `samples` - Input samples (mono)
/// * `from_rate` - Source sample rate in Hz (must be > 0)
/// * `to_rate` - Target sample rate in Hz
pub fn try_resample(
    samples: &[i16],
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<i16>, FormatError> {
    if from_rate == 0 {
        return Err(FormatError::ZeroSampleRate);
    }
    if from_rate == to_rate || samples.is_empty() {
        return Ok(samples.to_vec());
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

    Ok(output)
}

/// Resamples audio from one sample rate to another.
///
/// Uses linear interpolation, which is fast but may introduce artifacts
/// for large rate changes. Suitable for speech/transcription use cases.
///
/// # Panics
///
/// Panics if `from_rate` is zero.
///
/// For a non-panicking version, use [`try_resample`].
#[allow(clippy::expect_used)] // Intentional panic on invalid input
pub fn resample(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    try_resample(samples, from_rate, to_rate).expect("resample requires from_rate > 0")
}

/// Resamples stereo audio (checked version).
///
/// Processes left and right channels separately.
///
/// Returns `Err(FormatError::OddSampleCount)` if input has odd number of samples.
/// Returns `Err(FormatError::ZeroSampleRate)` if `from_rate` is zero.
pub fn try_resample_stereo(
    samples: &[i16],
    from_rate: u32,
    to_rate: u32,
) -> Result<Vec<i16>, FormatError> {
    if samples.len() % 2 != 0 {
        return Err(FormatError::OddSampleCount {
            count: samples.len(),
        });
    }
    if from_rate == 0 {
        return Err(FormatError::ZeroSampleRate);
    }
    if from_rate == to_rate || samples.is_empty() {
        return Ok(samples.to_vec());
    }

    // Deinterleave
    let mut left = Vec::with_capacity(samples.len() / 2);
    let mut right = Vec::with_capacity(samples.len() / 2);

    for chunk in samples.chunks_exact(2) {
        left.push(chunk[0]);
        right.push(chunk[1]);
    }

    // Resample each channel (from_rate is validated above, so unwrap is safe)
    let left_resampled = try_resample(&left, from_rate, to_rate)?;
    let right_resampled = try_resample(&right, from_rate, to_rate)?;

    // Interleave
    let mut output = Vec::with_capacity(left_resampled.len() * 2);
    for (l, r) in left_resampled.into_iter().zip(right_resampled) {
        output.push(l);
        output.push(r);
    }

    Ok(output)
}

/// Resamples stereo audio.
///
/// Processes left and right channels separately.
///
/// # Panics
///
/// Panics if input has odd number of samples or if `from_rate` is zero.
///
/// For a non-panicking version, use [`try_resample_stereo`].
#[allow(clippy::expect_used)] // Intentional panic on invalid input
pub fn resample_stereo(samples: &[i16], from_rate: u32, to_rate: u32) -> Vec<i16> {
    try_resample_stereo(samples, from_rate, to_rate)
        .expect("resample_stereo requires even samples and from_rate > 0")
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
    fn test_resample_zero_from_rate_returns_error() {
        // from_rate=0 should return an error
        let samples = vec![100i16];
        let result = try_resample(&samples, 0, 16000);
        assert_eq!(result, Err(crate::format::FormatError::ZeroSampleRate));
    }

    #[test]
    #[should_panic(expected = "from_rate > 0")]
    fn test_resample_zero_from_rate_panics() {
        // Verify the panicking wrapper works correctly
        let samples = vec![100i16];
        resample(&samples, 0, 16000);
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
    fn test_resample_stereo_odd_samples_returns_error() {
        // Odd number of stereo samples should return an error
        let odd_stereo = vec![100i16, 200, 300]; // 1.5 frames
        let result = try_resample_stereo(&odd_stereo, 16000, 32000);
        assert_eq!(
            result,
            Err(crate::format::FormatError::OddSampleCount { count: 3 })
        );
    }

    #[test]
    #[should_panic(expected = "even samples")]
    fn test_resample_stereo_odd_samples_panics() {
        // Verify the panicking wrapper works correctly
        let odd_stereo = vec![100i16, 200, 300];
        resample_stereo(&odd_stereo, 16000, 32000);
    }

    #[test]
    fn test_resample_stereo_single_sample_returns_error() {
        // Single sample should return an error
        let single = vec![100i16];
        let result = try_resample_stereo(&single, 16000, 32000);
        assert_eq!(
            result,
            Err(crate::format::FormatError::OddSampleCount { count: 1 })
        );
    }

    #[test]
    #[should_panic(expected = "even samples")]
    fn test_resample_stereo_single_sample_panics() {
        // Verify the panicking wrapper works correctly
        let single = vec![100i16];
        resample_stereo(&single, 16000, 32000);
    }

    #[test]
    fn test_resample_stereo_same_rate_odd_returns_error() {
        // Even same-rate passthrough should check for odd samples now
        let odd_stereo = vec![100i16, 200, 300];
        let result = try_resample_stereo(&odd_stereo, 16000, 16000);
        assert_eq!(
            result,
            Err(crate::format::FormatError::OddSampleCount { count: 3 })
        );
    }

    #[test]
    fn test_resample_stereo_same_rate_passthrough() {
        // Same rate with valid even samples should passthrough
        let stereo = vec![100i16, 200, 300, 400];
        let result = resample_stereo(&stereo, 16000, 16000);
        assert_eq!(result, vec![100, 200, 300, 400]);
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
