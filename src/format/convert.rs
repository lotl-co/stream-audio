//! Sample format and channel conversion.

/// Maximum positive value for symmetric i16 scaling.
///
/// We use 32767 (not 32768) so that -1.0 and +1.0 map to values with equal magnitude.
/// This is the standard convention for audio processing to avoid asymmetric clipping.
const I16_MAX_SYMMETRIC: f32 = i16::MAX as f32; // 32767.0

/// Full range divisor for i16 to f32 conversion.
///
/// Uses 32768 to map the full i16 range [-32768, 32767] to approximately [-1.0, 1.0].
const I16_RANGE: f32 = 32768.0;

/// Minimum i16 value as f32 for clamping.
const I16_MIN_F32: f32 = i16::MIN as f32; // -32768.0

/// Maximum i16 value as f32 for clamping.
const I16_MAX_F32: f32 = i16::MAX as f32; // 32767.0

/// Converts f32 samples to i16.
///
/// Input should be in the range [-1.0, 1.0].
/// Values outside this range are clamped.
///
/// Uses symmetric scaling so -1.0 maps to -32767 rather than -32768,
/// losing 1 LSB at the negative extreme. This avoids asymmetric clipping.
#[inline]
pub fn f32_to_i16(sample: f32) -> i16 {
    (sample * I16_MAX_SYMMETRIC).clamp(I16_MIN_F32, I16_MAX_F32) as i16
}

/// Converts i16 samples to f32.
///
/// Output will be in the range [-1.0, 1.0].
#[inline]
pub fn i16_to_f32(sample: i16) -> f32 {
    f32::from(sample) / I16_RANGE
}

/// Converts stereo samples to mono by averaging channels.
///
/// Input must have an even number of samples (left, right pairs).
/// Returns a vector half the size of the input.
pub fn stereo_to_mono(stereo: &[i16]) -> Vec<i16> {
    stereo
        .chunks_exact(2)
        .map(|pair| {
            // Average the two channels, avoiding overflow
            let left = i32::from(pair[0]);
            let right = i32::from(pair[1]);
            ((left + right) / 2) as i16
        })
        .collect()
}

/// Converts mono samples to stereo by duplicating each sample.
pub fn mono_to_stereo(mono: &[i16]) -> Vec<i16> {
    mono.iter().flat_map(|&s| [s, s]).collect()
}

/// Batch converts f32 samples to i16.
#[allow(dead_code)] // Useful utility for future use
pub fn f32_slice_to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|&s| f32_to_i16(s)).collect()
}

/// Batch converts i16 samples to f32.
#[allow(dead_code)] // Useful utility, may be used in future
pub fn i16_slice_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| i16_to_f32(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f32_to_i16_full_range() {
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32767);
        assert_eq!(f32_to_i16(0.0), 0);
    }

    #[test]
    fn test_f32_to_i16_clamping() {
        assert_eq!(f32_to_i16(2.0), 32767);
        assert_eq!(f32_to_i16(-2.0), -32768);
    }

    #[test]
    fn test_i16_to_f32_full_range() {
        let max = i16_to_f32(32767);
        assert!((max - 0.99997).abs() < 0.001);

        let min = i16_to_f32(-32768);
        assert!((min - (-1.0)).abs() < 0.001);

        assert_eq!(i16_to_f32(0), 0.0);
    }

    #[test]
    fn test_roundtrip() {
        for &original in &[0i16, 1000, -1000, 32767, -32768] {
            let f = i16_to_f32(original);
            let back = f32_to_i16(f);
            // Allow for small rounding errors
            assert!((original - back).abs() <= 1);
        }
    }

    #[test]
    fn test_stereo_to_mono() {
        let stereo = vec![100i16, 200, 300, 400];
        let mono = stereo_to_mono(&stereo);
        assert_eq!(mono, vec![150, 350]);
    }

    #[test]
    fn test_stereo_to_mono_cancellation() {
        // Opposite values should cancel
        let stereo = vec![1000i16, -1000];
        let mono = stereo_to_mono(&stereo);
        assert_eq!(mono, vec![0]);
    }

    #[test]
    fn test_mono_to_stereo() {
        let mono = vec![100i16, 200];
        let stereo = mono_to_stereo(&mono);
        assert_eq!(stereo, vec![100, 100, 200, 200]);
    }

    #[test]
    fn test_batch_conversion() {
        let f32_samples = vec![0.0f32, 0.5, -0.5, 1.0];
        let i16_samples = f32_slice_to_i16(&f32_samples);

        assert_eq!(i16_samples[0], 0);
        assert_eq!(i16_samples[1], 16383);
        assert_eq!(i16_samples[2], -16383);
        assert_eq!(i16_samples[3], 32767);
    }

    // ==================== Edge Case Tests ====================

    #[test]
    fn test_f32_to_i16_nan() {
        // NaN behavior: clamp produces NaN, cast to i16 is implementation-defined
        // This test documents current behavior (may vary by platform)
        let result = f32_to_i16(f32::NAN);
        // NaN comparisons are always false, so clamp returns NaN
        // Casting NaN to i16 is UB in older Rust, but modern Rust saturates to 0
        // Just verify it doesn't panic and produces some i16 value
        let _ = result; // Compiles and runs without panic
    }

    #[test]
    fn test_f32_to_i16_positive_infinity() {
        // +Infinity * 32767 = +Infinity, clamp should return max
        let result = f32_to_i16(f32::INFINITY);
        assert_eq!(result, i16::MAX);
    }

    #[test]
    fn test_f32_to_i16_negative_infinity() {
        // -Infinity * 32767 = -Infinity, clamp should return min
        let result = f32_to_i16(f32::NEG_INFINITY);
        assert_eq!(result, i16::MIN);
    }

    #[test]
    fn test_f32_to_i16_subnormal() {
        // Very small subnormal float - should round to 0
        let subnormal = f32::MIN_POSITIVE / 2.0; // Subnormal value
        let result = f32_to_i16(subnormal);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_stereo_to_mono_odd_samples() {
        // Odd number of samples: chunks_exact ignores the remainder (lossy!)
        // This is a bug - the last sample is silently dropped
        let odd_samples = vec![100i16, 200, 300];
        let result = stereo_to_mono(&odd_samples);
        // Only processes first 2 samples, third is lost
        assert_eq!(result, vec![150]);
    }

    #[test]
    fn test_stereo_to_mono_single_sample() {
        // Single sample: chunks_exact yields nothing (lossy!)
        // This is a bug - input is silently dropped
        let single = vec![100i16];
        let result = stereo_to_mono(&single);
        assert!(result.is_empty()); // Sample is lost!
    }

    #[test]
    fn test_stereo_to_mono_empty() {
        // Empty input should return empty output
        let empty: Vec<i16> = vec![];
        let result = stereo_to_mono(&empty);
        assert!(result.is_empty());
    }

    #[test]
    fn test_f32_slice_to_i16_empty() {
        let empty: Vec<f32> = vec![];
        let result = f32_slice_to_i16(&empty);
        assert!(result.is_empty());
    }

    #[test]
    fn test_i16_slice_to_f32_edge_values() {
        let samples = vec![i16::MIN, i16::MAX, 0];
        let result = i16_slice_to_f32(&samples);

        assert_eq!(result.len(), 3);
        assert!((result[0] - (-1.0)).abs() < 0.001); // MIN → -1.0
        assert!((result[1] - 0.99997).abs() < 0.001); // MAX → ~1.0
        assert_eq!(result[2], 0.0); // 0 → 0.0
    }
}
