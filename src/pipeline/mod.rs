//! Audio pipeline components.
//!
//! The pipeline connects the audio source to sinks via a ring buffer:
//!
//! ```text
//! CPAL Thread → Ring Buffer → Capture Bridge → Router Task → Sinks
//! ```
//!
//! - **Ring Buffer**: Lock-free SPSC queue absorbs pressure from slow sinks
//! - **Capture Bridge**: Reads from buffer, converts format, forwards to router
//! - **Router**: Fans out chunks to all registered sinks with retry logic
//! - **Routing**: Configures which sources send to which sinks
//!
//! The ring buffer ensures the CPAL callback never blocks.

mod capture;
mod ring_buffer;
mod router;
mod routing;

pub(crate) use capture::{spawn_capture_bridge, CaptureConfig};
pub(crate) use ring_buffer::AudioBuffer;
pub(crate) use router::{Router, RouterCommand};
pub(crate) use routing::{MergeGroup, RoutingTable, SinkRoute, SinkRouteBuilder};
