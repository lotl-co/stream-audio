//! Sample format and channel conversion.

/// Converts f32 samples to i16.
///
/// Input should be in the range [-1.0, 1.0].
/// Values outside this range are clamped.
///
/// Uses × 32767 (not 32768) for symmetric scaling. This means -1.0 maps
/// to -32767 rather than -32768, losing 1 LSB at the negative extreme.
/// This is a common convention that avoids producing out-of-range values.
#[inline]
pub fn f32_to_i16(sample: f32) -> i16 {
    (sample * 32767.0).clamp(-32768.0, 32767.0) as i16
}

/// Converts i16 samples to f32.
///
/// Output will be in the range [-1.0, 1.0].
#[inline]
pub fn i16_to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
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
#[allow(dead_code)] // Useful utility for future use
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
}
