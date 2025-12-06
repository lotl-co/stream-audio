//! Sink trait and implementations for audio destinations.
//!
//! A [`Sink`] is any destination that can receive audio chunks. The crate
//! provides two built-in sinks:
//!
//! - [`ChannelSink`]: Sends chunks to a tokio mpsc channel
//! - [`FileSink`]: Writes chunks to a WAV file
//!
//! You can implement the [`Sink`] trait for custom destinations like
//! network endpoints or audio processors.

mod channel;
mod file;

pub use channel::ChannelSink;
pub use file::FileSink;

use crate::{AudioChunk, SinkError};
use async_trait::async_trait;

/// A destination for audio data.
///
/// Sinks receive audio chunks from the router and process them (write to
/// file, send over network, forward to channel, etc.).
///
/// # Implementation Notes
///
/// - Methods take `&self` - use interior mutability (`Mutex`, `RwLock`) if needed
/// - All methods are async and run on the tokio runtime
/// - `on_start` is called before any audio flows; open resources here
/// - `on_stop` is called during graceful shutdown; close resources here
/// - `write` may be called concurrently with retries; ensure thread safety
///
/// # Example
///
/// ```
/// use stream_audio::{Sink, AudioChunk, SinkError};
/// use async_trait::async_trait;
///
/// struct PrintSink {
///     name: String,
/// }
///
/// #[async_trait]
/// impl Sink for PrintSink {
///     fn name(&self) -> &str {
///         &self.name
///     }
///
///     async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError> {
///         println!("Received {} samples", chunk.samples.len());
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Sink: Send + Sync {
    /// Human-readable name for logging and error messages.
    fn name(&self) -> &str;

    /// Called once before streaming begins.
    ///
    /// Use this to open files, establish connections, or allocate resources.
    /// Errors here are fatal and will prevent the stream from starting.
    ///
    /// Default implementation does nothing.
    async fn on_start(&self) -> Result<(), SinkError> {
        Ok(())
    }

    /// Write a chunk of audio samples.
    ///
    /// This is the main method called for each chunk of audio. Implementations
    /// should handle their own buffering if needed.
    ///
    /// Errors are recoverable - the router will emit a [`StreamEvent::SinkError`]
    /// and may retry based on [`StreamConfig`] settings.
    ///
    /// [`StreamEvent::SinkError`]: crate::StreamEvent::SinkError
    /// [`StreamConfig`]: crate::StreamConfig
    async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError>;

    /// Called during graceful shutdown.
    ///
    /// Use this to flush buffers, close files, or clean up resources.
    /// This is called even if errors occurred during streaming.
    ///
    /// Default implementation does nothing.
    async fn on_stop(&self) -> Result<(), SinkError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct CountingSink {
        name: String,
        count: AtomicUsize,
    }

    impl CountingSink {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                count: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Sink for CountingSink {
        fn name(&self) -> &str {
            &self.name
        }

        async fn write(&self, _chunk: &AudioChunk) -> Result<(), SinkError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_sink_lifecycle() {
        let sink = CountingSink::new("test");

        // Start should succeed
        sink.on_start().await.unwrap();

        // Write some chunks
        let chunk = AudioChunk::new(vec![0i16; 100], Duration::ZERO, 16000, 1);
        sink.write(&chunk).await.unwrap();
        sink.write(&chunk).await.unwrap();

        assert_eq!(sink.count(), 2);

        // Stop should succeed
        sink.on_stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_sink_name() {
        let sink = CountingSink::new("my-sink");
        assert_eq!(sink.name(), "my-sink");
    }

    #[test]
    fn test_sink_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<dyn Sink>>();
    }
}
