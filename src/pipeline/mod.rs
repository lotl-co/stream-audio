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

pub(crate) use ring_buffer::AudioBuffer;
pub(crate) use router::{Router, RouterCommand};
