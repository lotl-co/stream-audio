//! Tokio mpsc channel sink implementation.

use crate::sink::Sink;
use crate::{AudioChunk, SinkError};
use async_trait::async_trait;
use tokio::sync::mpsc;

/// A sink that sends audio chunks to a tokio mpsc channel.
///
/// This is the primary way to receive audio data for processing
/// (transcription, analysis, etc.).
///
/// # Example
///
/// ```
/// use stream_audio::{AudioChunk, ChannelSink};
/// use tokio::sync::mpsc;
///
/// let (tx, mut rx) = mpsc::channel::<AudioChunk>(100);
/// let sink = ChannelSink::new(tx);
///
/// // Use sink with StreamAudio builder...
/// // Then receive chunks:
/// // while let Some(chunk) = rx.recv().await { ... }
/// ```
pub struct ChannelSink {
    name: String,
    sender: mpsc::Sender<AudioChunk>,
}

impl ChannelSink {
    /// Creates a new channel sink with the given sender.
    ///
    /// The sender should have sufficient buffer capacity for your use case.
    /// A capacity of 100 is typically sufficient for most applications.
    pub fn new(sender: mpsc::Sender<AudioChunk>) -> Self {
        Self {
            name: "channel".to_string(),
            sender,
        }
    }

    /// Creates a new channel sink with a custom name.
    pub fn with_name(name: impl Into<String>, sender: mpsc::Sender<AudioChunk>) -> Self {
        Self {
            name: name.into(),
            sender,
        }
    }
}

#[async_trait]
impl Sink for ChannelSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError> {
        self.sender
            .send(chunk.clone())
            .await
            .map_err(|_| SinkError::ChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_channel_sink_sends_chunks() {
        let (tx, mut rx) = mpsc::channel::<AudioChunk>(10);
        let sink = ChannelSink::new(tx);

        let chunk = AudioChunk::new(vec![1, 2, 3], Duration::ZERO, 16000, 1);
        sink.write(&chunk).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received.samples, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_channel_sink_closed() {
        let (tx, rx) = mpsc::channel::<AudioChunk>(10);
        let sink = ChannelSink::new(tx);

        // Drop the receiver
        drop(rx);

        let chunk = AudioChunk::new(vec![1, 2, 3], Duration::ZERO, 16000, 1);
        let result = sink.write(&chunk).await;

        assert!(matches!(result, Err(SinkError::ChannelClosed)));
    }

    #[tokio::test]
    async fn test_channel_sink_custom_name() {
        let (tx, _rx) = mpsc::channel::<AudioChunk>(10);
        let sink = ChannelSink::with_name("transcription", tx);
        assert_eq!(sink.name(), "transcription");
    }
}
