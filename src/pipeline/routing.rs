//! Sink routing configuration for multi-source capture.
//!
//! This module defines how audio chunks from sources are routed to sinks.
//! Sinks can receive audio from:
//! - A single source (direct routing)
//! - Multiple sources merged together (merge routing)
//! - All sources (broadcast routing, default for backward compatibility)

use std::collections::{HashMap, HashSet};

use crate::source::SourceId;
use crate::StreamAudioError;

/// Specifies which sources a sink should receive audio from.
#[derive(Debug, Clone)]
pub enum SinkRoute {
    /// Receive audio from each source independently (broadcast).
    ///
    /// Each source's chunks are sent separately to this sink. Use this when
    /// the sink should process audio from all sources without merging.
    Broadcast,

    /// Receive audio from a single source only.
    Single(SourceId),

    /// Receive merged audio from multiple sources.
    Merged(HashSet<SourceId>),
}

impl Default for SinkRoute {
    fn default() -> Self {
        Self::Broadcast
    }
}

impl SinkRoute {
    /// Creates a route for a single source.
    #[allow(dead_code)] // Public API for external use
    pub fn single(source_id: impl Into<SourceId>) -> Self {
        Self::Single(source_id.into())
    }

    /// Creates a route for merged sources.
    pub fn merged<I, S>(sources: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SourceId>,
    {
        Self::Merged(sources.into_iter().map(Into::into).collect())
    }

    /// Returns true if this route wants chunks from the given source.
    #[allow(dead_code)] // Public API for external use
    pub fn wants_source(&self, source_id: &SourceId) -> bool {
        match self {
            Self::Broadcast => true,
            Self::Single(id) => id == source_id,
            Self::Merged(ids) => ids.contains(source_id),
        }
    }

    /// Returns true if this is a merge route.
    #[allow(dead_code)] // Public API for external use
    pub fn is_merged(&self) -> bool {
        matches!(self, Self::Merged(_))
    }

    /// Returns the set of source IDs this route needs, if known.
    #[allow(dead_code)] // Public API for external use
    pub fn required_sources(&self) -> Option<&HashSet<SourceId>> {
        match self {
            Self::Merged(ids) => Some(ids),
            _ => None,
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

    /// Merge configurations: (source set, sink indices).
    merge_groups: Vec<MergeGroup>,

    /// All known source IDs (for introspection).
    #[allow(dead_code)]
    source_ids: HashSet<SourceId>,
}

/// A group of sources that are merged and sent to specific sinks.
#[derive(Debug, Clone)]
pub struct MergeGroup {
    /// The sources to merge (for introspection).
    #[allow(dead_code)]
    pub sources: HashSet<SourceId>,
    /// Indices of sinks that want this merged output.
    pub sink_indices: Vec<usize>,
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
        let mut merge_map: HashMap<Vec<SourceId>, Vec<usize>> = HashMap::new();

        for (sink_idx, route) in routes {
            match route {
                SinkRoute::Broadcast => {
                    broadcast_sinks.push(sink_idx);
                }
                SinkRoute::Single(source_id) => {
                    if !source_ids.contains(source_id) {
                        return Err(StreamAudioError::UnknownSourceInRoute {
                            sink_name: format!("sink_{sink_idx}"),
                            source_id: source_id.to_string(),
                        });
                    }
                    direct_routes
                        .entry(source_id.clone())
                        .or_default()
                        .push(sink_idx);
                }
                SinkRoute::Merged(sources) => {
                    // Validate all sources exist
                    for source_id in sources {
                        if !source_ids.contains(source_id) {
                            return Err(StreamAudioError::UnknownSourceInRoute {
                                sink_name: format!("sink_{sink_idx}"),
                                source_id: source_id.to_string(),
                            });
                        }
                    }
                    // Use sorted vec as key for deduplication
                    let mut key: Vec<SourceId> = sources.iter().cloned().collect();
                    key.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                    merge_map.entry(key).or_default().push(sink_idx);
                }
            }
        }

        // Convert merge_map to merge_groups
        let merge_groups = merge_map
            .into_iter()
            .map(|(sources, sink_indices)| MergeGroup {
                sources: sources.into_iter().collect(),
                sink_indices,
            })
            .collect();

        Ok(Self {
            direct_routes,
            broadcast_sinks,
            merge_groups,
            source_ids,
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

    /// Returns all merge groups.
    pub fn merge_groups(&self) -> &[MergeGroup] {
        &self.merge_groups
    }

    /// Returns all known source IDs (for introspection).
    #[allow(dead_code)]
    pub fn source_ids(&self) -> &HashSet<SourceId> {
        &self.source_ids
    }

    /// Returns true if there are any merge routes.
    pub fn has_merge_routes(&self) -> bool {
        !self.merge_groups.is_empty()
    }

    /// Returns true if this is a single-source setup (backward compatibility).
    #[allow(dead_code)]
    pub fn is_single_source(&self) -> bool {
        self.source_ids.len() <= 1
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
        assert!(!route.is_merged());
    }

    #[test]
    fn test_sink_route_single() {
        let route = SinkRoute::single("mic");
        assert!(route.wants_source(&SourceId::new("mic")));
        assert!(!route.wants_source(&SourceId::new("speaker")));
        assert!(!route.is_merged());
    }

    #[test]
    fn test_sink_route_merged() {
        let route = SinkRoute::merged(["mic", "speaker"]);
        assert!(route.wants_source(&SourceId::new("mic")));
        assert!(route.wants_source(&SourceId::new("speaker")));
        assert!(!route.wants_source(&SourceId::new("other")));
        assert!(route.is_merged());
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
    fn test_routing_table_merged() {
        let sources = vec![SourceId::new("mic"), SourceId::new("speaker")];
        let routes = vec![(0, SinkRoute::merged(["mic", "speaker"]))];

        let table = RoutingTable::new(routes.iter().map(|(i, r)| (*i, r)), sources).unwrap();

        assert!(table.has_merge_routes());
        assert_eq!(table.merge_groups().len(), 1);
        assert_eq!(table.merge_groups()[0].sink_indices, vec![0]);
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
