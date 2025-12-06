//! Builder pattern for StreamAudio.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::pipeline::{AudioBuffer, Router};
use crate::session::{Session, SessionState};
use crate::sink::Sink;
use crate::source::AudioDevice;
use crate::{
    event_callback, EventCallback, FormatPreset, StreamAudioError, StreamConfig, StreamEvent,
};

/// Builder for configuring and starting audio capture.
///
/// Use [`StreamAudio::builder()`] to create a new builder.
///
/// # Example
///
/// ```ignore
/// use stream_audio::{StreamAudio, FileSink, ChannelSink, FormatPreset};
/// use tokio::sync::mpsc;
///
/// let (tx, rx) = mpsc::channel(100);
///
/// let session = StreamAudio::builder()
///     .format(FormatPreset::Transcription)
///     .add_sink(FileSink::wav("output.wav"))
///     .add_sink(ChannelSink::new(tx))
///     .start()
///     .await?;
/// ```
///
/// [`StreamAudio::builder()`]: crate::StreamAudio::builder
pub struct StreamAudioBuilder {
    device_name: Option<String>,
    format: FormatPreset,
    sinks: Vec<Arc<dyn Sink>>,
    event_callback: Option<EventCallback>,
    config: StreamConfig,
}

impl Default for StreamAudioBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamAudioBuilder {
    /// Creates a new builder with default settings.
    pub fn new() -> Self {
        Self {
            device_name: None,
            format: FormatPreset::default(),
            sinks: Vec::new(),
            event_callback: None,
            config: StreamConfig::default(),
        }
    }

    /// Use the default input device.
    ///
    /// This is the default behavior if no device is specified.
    pub fn device_default(mut self) -> Self {
        self.device_name = None;
        self
    }

    /// Use a specific input device by name.
    ///
    /// Use [`list_input_devices()`](crate::list_input_devices) to get available device names.
    pub fn device(mut self, name: impl Into<String>) -> Self {
        self.device_name = Some(name.into());
        self
    }

    /// Set the audio format preset.
    ///
    /// Default: [`FormatPreset::Transcription`] (16kHz mono)
    pub fn format(mut self, format: FormatPreset) -> Self {
        self.format = format;
        self
    }

    /// Add a sink to receive audio data.
    ///
    /// At least one sink must be added before calling `start()`.
    pub fn add_sink<S: Sink + 'static>(mut self, sink: S) -> Self {
        self.sinks.push(Arc::new(sink));
        self
    }

    /// Set a callback to receive runtime events.
    ///
    /// Events include buffer overflow warnings, sink errors, and stream interruptions.
    pub fn on_event<F>(mut self, callback: F) -> Self
    where
        F: Fn(StreamEvent) + Send + Sync + 'static,
    {
        self.event_callback = Some(event_callback(callback));
        self
    }

    /// Set custom stream configuration.
    pub fn with_config(mut self, config: StreamConfig) -> Self {
        self.config = config;
        self
    }

    /// Start audio capture.
    ///
    /// Returns a [`Session`] handle to control the capture.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No sinks are configured
    /// - The audio device cannot be opened
    /// - Any sink fails to start
    pub async fn start(self) -> Result<Session, StreamAudioError> {
        // Validate configuration
        if self.sinks.is_empty() {
            return Err(StreamAudioError::NoSinksConfigured);
        }

        // Open audio device
        let device = match &self.device_name {
            Some(name) => AudioDevice::open_by_name(name)?,
            None => AudioDevice::open_default()?,
        };

        // Get device config
        let device_config = device.config();
        let sample_rate = self
            .format
            .sample_rate()
            .unwrap_or(device_config.sample_rate);
        let channels = self.format.channels().unwrap_or(device_config.channels);

        // Create channels for communication
        let (chunk_tx, chunk_rx) = mpsc::channel(100);
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(1);

        // Create shared state
        let state = Arc::new(SessionState::new());
        let state_capture = state.clone();

        // Create and start router
        let mut router = Router::new(self.sinks.clone(), self.config.clone());
        if let Some(callback) = self.event_callback.clone() {
            router = router.with_event_callback(callback);
        }

        // Start sinks
        router.start_sinks().await?;

        // Spawn router task
        let router_handle = tokio::spawn(async move {
            router.run(chunk_rx, router_cmd_rx).await;
        });

        // Start CPAL capture - returns the ring buffer consumer
        let (capture_stream, ring_consumer) = device.start_capture()?;

        // Wrap the consumer in an AudioBuffer for chunk-based reading
        let mut audio_buffer = AudioBuffer::new(
            ring_consumer,
            sample_rate,
            channels,
            self.config.chunk_duration,
        );

        // Spawn capture -> router bridge task
        let chunk_duration = self.config.chunk_duration;
        let capture_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(chunk_duration / 2);

            while state_capture.running.load(Ordering::SeqCst) {
                interval.tick().await;

                // Read from ring buffer consumer and create chunks
                while audio_buffer.has_chunk() {
                    if let Some(chunk) = audio_buffer.try_read_chunk() {
                        state_capture
                            .samples_captured
                            .fetch_add(chunk.samples.len() as u64, Ordering::SeqCst);
                        state_capture
                            .chunks_processed
                            .fetch_add(1, Ordering::SeqCst);

                        if chunk_tx.send(chunk).await.is_err() {
                            // Router closed, stop capture
                            break;
                        }
                    }
                }
            }

            // Drain remaining audio
            for chunk in audio_buffer.drain() {
                let _ = chunk_tx.send(chunk).await;
            }
        });

        Ok(Session::new(
            state,
            router_cmd_tx,
            router_handle,
            capture_handle,
            capture_stream,
        ))
    }
}

/// Main entry point for stream-audio.
///
/// Use [`StreamAudio::builder()`] to start configuring audio capture.
pub struct StreamAudio;

impl StreamAudio {
    /// Creates a new builder for configuring audio capture.
    pub fn builder() -> StreamAudioBuilder {
        StreamAudioBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_default() {
        let builder = StreamAudioBuilder::new();
        assert!(builder.device_name.is_none());
        assert!(builder.sinks.is_empty());
    }

    #[test]
    fn test_builder_device() {
        let builder = StreamAudio::builder().device("My Microphone");
        assert_eq!(builder.device_name, Some("My Microphone".to_string()));
    }

    #[test]
    fn test_builder_format() {
        let builder = StreamAudio::builder().format(FormatPreset::Native);
        assert_eq!(builder.format, FormatPreset::Native);
    }
}
