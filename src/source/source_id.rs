//! Source identification type.

use std::sync::Arc;

/// Unique identifier for an audio source.
///
/// `SourceId` is a lightweight, cloneable identifier used to track which
/// audio device produced a given chunk. It uses `Arc<str>` internally for
/// efficient cloning and comparison.
///
/// # Performance
///
/// Cloning a `SourceId` is cheap (Arc pointer copy, no heap allocation).
///
/// # Example
///
/// ```
/// use stream_audio::SourceId;
///
/// let mic = SourceId::new("mic");
/// let speaker = SourceId::new("speaker");
///
/// assert_ne!(mic, speaker);
/// assert_eq!(mic, SourceId::new("mic"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(Arc<str>);

impl SourceId {
    /// Creates a new source ID from a string.
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }

    /// Returns the ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for SourceId {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<String> for SourceId {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl AsRef<str> for SourceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_id_equality() {
        let a = SourceId::new("mic");
        let b = SourceId::new("mic");
        let c = SourceId::new("speaker");

        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_source_id_display() {
        let id = SourceId::new("microphone");
        assert_eq!(format!("{id}"), "microphone");
    }

    #[test]
    fn test_source_id_from_str() {
        let id: SourceId = "test".into();
        assert_eq!(id.as_str(), "test");
    }

    #[test]
    fn test_source_id_from_string() {
        let id: SourceId = String::from("test").into();
        assert_eq!(id.as_str(), "test");
    }

    #[test]
    fn test_source_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(SourceId::new("mic"));
        set.insert(SourceId::new("speaker"));
        set.insert(SourceId::new("mic")); // duplicate

        assert_eq!(set.len(), 2);
    }
}
