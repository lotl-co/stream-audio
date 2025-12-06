//! Audio format conversion utilities.
//!
//! This module provides utilities for converting between audio formats:
//! - Sample format conversion (f32 ↔ i16)
//! - Channel conversion (stereo ↔ mono)
//! - Sample rate conversion (resampling)

mod convert;
mod resample;

pub use convert::{f32_to_i16, i16_to_f32, stereo_to_mono};
pub use resample::resample;
