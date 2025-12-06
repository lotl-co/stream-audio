//! Audio pipeline components.
//!
//! The pipeline connects the audio source to sinks via a ring buffer:
//!
//! ```text
//! CPAL Thread → Ring Buffer → Router Task → Sinks
//! ```
//!
//! The ring buffer absorbs pressure from slow sinks, ensuring the
//! CPAL callback never blocks.

mod ring_buffer;
mod router;

// These exports will be used by the builder/session modules
#[allow(unused_imports)]
pub(crate) use ring_buffer::{create_audio_buffer, AudioBuffer};
#[allow(unused_imports)]
pub(crate) use router::{Router, RouterCommand};
