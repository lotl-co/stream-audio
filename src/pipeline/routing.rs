//! Sink routing configuration for multi-source capture.
//!
//! This module defines how audio chunks from sources are routed to sinks.
//! Sinks can receive audio from:
//! - A single source (direct routing)
//! - All sources (broadcast routing, default for backward compatibility)

use std::collections::{HashMap, HashSet};

use crate::source::SourceId;
use crate::StreamAudioError;

/// Specifies which sources a sink should receive audio from.
#[derive(Debug, Clone, Default)]
pub enum SinkRoute {
    /// Receive audio from each source independently (broadcast).
    ///
    /// Each source's chunks are sent separately to this sink. Use this when
    /// the sink should process audio from all sources without merging.
    #[default]
    Broadcast,

    /// Receive audio from a single source only.
    Single(SourceId),
}

impl SinkRoute {
    /// Creates a route for a single source (test helper).
    #[cfg(test)]
    pub fn single(source_id: impl Into<SourceId>) -> Self {
        Self::Single(source_id.into())
    }

    /// Returns true if this route wants chunks from the given source (test helper).
    #[cfg(test)]
    pub fn wants_source(&self, source_id: &SourceId) -> bool {
        match self {
            Self::Broadcast => true,
            Self::Single(id) => id == source_id,
        }
    }
}

/// Pre-computed routing table for efficient chunk dispatch.
///
/// Built once at startup from sink configurations. Provides O(1) lookup
/// for routing decisions.
#[derive(Debug)]
pub struct RoutingTable {
    /// Maps `source_id` to list of sink indices that want direct chunks from this source.
    direct_routes: HashMap<SourceId, Vec<usize>>,

    /// Sink indices that want all sources (broadcast).
    broadcast_sinks: Vec<usize>,
}

impl RoutingTable {
    /// Creates a new routing table from sink routes.
    ///
    /// # Arguments
    ///
    /// * `routes` - Iterator of `(sink_index, route)` pairs
    /// * `source_ids` - All known source IDs
    ///
    /// # Errors
    ///
    /// Returns an error if a route references an unknown source.
    pub fn new<'a>(
        routes: impl IntoIterator<Item = (usize, &'a SinkRoute)>,
        source_ids: impl IntoIterator<Item = SourceId>,
    ) -> Result<Self, StreamAudioError> {
        let source_ids: HashSet<SourceId> = source_ids.into_iter().collect();
        let mut direct_routes: HashMap<SourceId, Vec<usize>> = HashMap::new();
        let mut broadcast_sinks = Vec::new();

        for (sink_idx, route) in routes {
            match route {
                SinkRoute::Broadcast => {
                    broadcast_sinks.push(sink_idx);
                }
                SinkRoute::Single(source_id) => {
                    if !source_ids.contains(source_id) {
                        return Err(StreamAudioError::UnknownSourceInRoute {
                            source_id: source_id.to_string(),
                        });
                    }
                    direct_routes
                        .entry(source_id.clone())
                        .or_default()
                        .push(sink_idx);
                }
            }
        }

        Ok(Self {
            direct_routes,
            broadcast_sinks,
        })
    }

    /// Returns sink indices that should receive direct chunks from a source.
    pub fn direct_sinks(&self, source_id: &SourceId) -> &[usize] {
        self.direct_routes.get(source_id).map_or(&[], Vec::as_slice)
    }

    /// Returns sink indices that want all sources.
    pub fn broadcast_sinks(&self) -> &[usize] {
        &self.broadcast_sinks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sink_route_broadcast() {
        let route = SinkRoute::Broadcast;
        assert!(route.wants_source(&SourceId::new("mic")));
        assert!(route.wants_source(&SourceId::new("speaker")));
    }

    #[test]
    fn test_sink_route_single() {
        let route = SinkRoute::single("mic");
        assert!(route.wants_source(&SourceId::new("mic")));
        assert!(!route.wants_source(&SourceId::new("speaker")));
    }

    #[test]
    fn test_routing_table_direct() {
        let sources = vec![SourceId::new("mic"), SourceId::new("speaker")];
        let routes = vec![
            (0, SinkRoute::single("mic")),
            (1, SinkRoute::single("speaker")),
        ];

        let table =
            RoutingTable::new(routes.iter().map(|(i, r)| (*i, r)), sources.clone()).unwrap();

        assert_eq!(table.direct_sinks(&SourceId::new("mic")), &[0]);
        assert_eq!(table.direct_sinks(&SourceId::new("speaker")), &[1]);
        assert!(table.broadcast_sinks().is_empty());
    }

    #[test]
    fn test_routing_table_broadcast() {
        let sources = vec![SourceId::new("mic")];
        let routes = vec![(0, SinkRoute::Broadcast), (1, SinkRoute::Broadcast)];

        let table = RoutingTable::new(routes.iter().map(|(i, r)| (*i, r)), sources).unwrap();

        assert_eq!(table.broadcast_sinks(), &[0, 1]);
    }

    #[test]
    fn test_routing_table_unknown_source() {
        let sources = vec![SourceId::new("mic")];
        let routes = vec![(0, SinkRoute::single("unknown"))];

        let result = RoutingTable::new(routes.iter().map(|(i, r)| (*i, r)), sources);

        assert!(matches!(
            result,
            Err(StreamAudioError::UnknownSourceInRoute { .. })
        ));
    }
}
