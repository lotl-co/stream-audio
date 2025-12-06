//! Audio format conversion utilities.
//!
//! This module provides utilities for converting between audio formats:
//! - Sample format conversion (f32 ↔ i16)
//! - Channel conversion (stereo ↔ mono)
//! - Sample rate conversion (resampling)

mod convert;
mod resample;

pub use convert::{f32_to_i16, i16_to_f32, mono_to_stereo, stereo_to_mono};
pub use resample::{resample, resample_stereo};

/// Converts audio between formats (resampling + channel conversion).
///
/// This handles the conversion from device-native format to target format,
/// performing channel conversion first (fewer samples to resample), then resampling.
#[derive(Debug, Clone)]
pub struct FormatConverter {
    source_rate: u32,
    source_channels: u16,
    target_rate: u32,
    target_channels: u16,
}

impl FormatConverter {
    /// Creates a new format converter.
    pub fn new(
        source_rate: u32,
        source_channels: u16,
        target_rate: u32,
        target_channels: u16,
    ) -> Self {
        Self {
            source_rate,
            source_channels,
            target_rate,
            target_channels,
        }
    }

    /// Returns true if no conversion is needed (formats match).
    pub fn is_passthrough(&self) -> bool {
        self.source_rate == self.target_rate && self.source_channels == self.target_channels
    }

    /// Converts samples from source format to target format.
    ///
    /// Order: channel conversion first (stereo→mono), then resample.
    /// This minimizes work since we resample fewer samples after downmixing.
    pub fn convert(&self, samples: &[i16]) -> Vec<i16> {
        if self.is_passthrough() {
            return samples.to_vec();
        }

        let mut data = samples.to_vec();

        // Step 1: Channel conversion
        if self.source_channels == 2 && self.target_channels == 1 {
            data = stereo_to_mono(&data);
        } else if self.source_channels == 1 && self.target_channels == 2 {
            data = mono_to_stereo(&data);
        }

        // Step 2: Resample
        if self.source_rate != self.target_rate {
            // After channel conversion, we're at target_channels
            if self.target_channels == 2 {
                data = resample_stereo(&data, self.source_rate, self.target_rate);
            } else {
                data = resample(&data, self.source_rate, self.target_rate);
            }
        }

        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough() {
        let converter = FormatConverter::new(48000, 2, 48000, 2);
        assert!(converter.is_passthrough());

        let samples = vec![100, 200, 300, 400];
        let result = converter.convert(&samples);
        assert_eq!(result, samples);
    }

    #[test]
    fn test_stereo_to_mono_only() {
        let converter = FormatConverter::new(16000, 2, 16000, 1);
        assert!(!converter.is_passthrough());

        // Stereo pairs: (100, 200), (300, 400)
        let samples = vec![100, 200, 300, 400];
        let result = converter.convert(&samples);
        // Mono averages: 150, 350
        assert_eq!(result, vec![150, 350]);
    }

    #[test]
    fn test_resample_only() {
        let converter = FormatConverter::new(48000, 1, 16000, 1);
        assert!(!converter.is_passthrough());

        // 48kHz mono -> 16kHz mono (3:1 ratio)
        // 6 samples at 48kHz = 2 samples at 16kHz
        let samples = vec![0, 100, 200, 300, 400, 500];
        let result = converter.convert(&samples);
        // Should be approximately 2 samples (may be 3 due to ceiling)
        assert!(result.len() <= 3);
    }

    #[test]
    fn test_stereo_to_mono_and_resample() {
        let converter = FormatConverter::new(48000, 2, 16000, 1);
        assert!(!converter.is_passthrough());

        // 12 stereo samples at 48kHz = 6 frames
        // -> 6 mono samples after stereo_to_mono
        // -> 2 mono samples after 3:1 resample
        let samples: Vec<i16> = (0..12).map(|i| i * 100).collect();
        let result = converter.convert(&samples);

        // Should produce approximately 2 samples
        assert!(result.len() >= 2 && result.len() <= 3);
    }

    #[test]
    fn test_mono_to_stereo() {
        let converter = FormatConverter::new(16000, 1, 16000, 2);
        assert!(!converter.is_passthrough());

        let samples = vec![100, 200];
        let result = converter.convert(&samples);
        assert_eq!(result, vec![100, 100, 200, 200]);
    }
}
