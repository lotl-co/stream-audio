//! Builder pattern for `StreamAudio`.

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

/// Specifies which audio input device to use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelection {
    /// Use the system's default input device.
    #[default]
    SystemDefault,
    /// Use a specific device by name.
    ByName(String),
}

/// Resolved audio configuration after opening the device.
struct ResolvedAudioConfig {
    device: AudioDevice,
    sample_rate: u32,
    channels: u16,
}

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
#[must_use]
pub struct StreamAudioBuilder {
    device: DeviceSelection,
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
            device: DeviceSelection::default(),
            format: FormatPreset::default(),
            sinks: Vec::new(),
            event_callback: None,
            config: StreamConfig::default(),
        }
    }

    /// Use the system's default input device.
    ///
    /// This is the default behavior if no device is specified.
    pub fn device_default(mut self) -> Self {
        self.device = DeviceSelection::SystemDefault;
        self
    }

    /// Use a specific input device by name.
    ///
    /// Use [`list_input_devices()`](crate::list_input_devices) to get available device names.
    pub fn device(mut self, name: impl Into<String>) -> Self {
        self.device = DeviceSelection::ByName(name.into());
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

    /// Validates the builder configuration.
    fn validate(&self) -> Result<(), StreamAudioError> {
        if self.sinks.is_empty() {
            return Err(StreamAudioError::NoSinksConfigured);
        }
        Ok(())
    }

    /// Opens the audio device and resolves the final audio parameters.
    fn resolve_device(&self) -> Result<ResolvedAudioConfig, StreamAudioError> {
        let device = match &self.device {
            DeviceSelection::SystemDefault => AudioDevice::open_default()?,
            DeviceSelection::ByName(name) => AudioDevice::open_by_name(name)?,
        };

        let device_config = device.config();
        let sample_rate = self
            .format
            .sample_rate()
            .unwrap_or(device_config.sample_rate);
        let channels = self.format.channels().unwrap_or(device_config.channels);

        Ok(ResolvedAudioConfig {
            device,
            sample_rate,
            channels,
        })
    }

    /// Creates and starts the router with all configured sinks.
    async fn start_router(
        &self,
        chunk_rx: mpsc::Receiver<crate::AudioChunk>,
        cmd_rx: mpsc::Receiver<crate::pipeline::RouterCommand>,
    ) -> Result<tokio::task::JoinHandle<()>, StreamAudioError> {
        let mut router = Router::new(self.sinks.clone(), self.config.clone());
        if let Some(callback) = self.event_callback.clone() {
            router = router.with_event_callback(callback);
        }

        router.start_sinks().await?;

        let handle = tokio::spawn(async move {
            router.run(chunk_rx, cmd_rx).await;
        });

        Ok(handle)
    }

    /// Spawns the capture bridge task that reads from the ring buffer and sends chunks.
    fn spawn_capture_bridge(
        &self,
        audio_config: &ResolvedAudioConfig,
        ring_consumer: ringbuf::HeapCons<i16>,
        chunk_tx: mpsc::Sender<crate::AudioChunk>,
        state: Arc<SessionState>,
    ) -> tokio::task::JoinHandle<()> {
        let mut audio_buffer = AudioBuffer::new(
            ring_consumer,
            audio_config.sample_rate,
            audio_config.channels,
            self.config.chunk_duration,
        );

        let chunk_duration = self.config.chunk_duration;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(chunk_duration / 2);

            while state.running.load(Ordering::SeqCst) {
                interval.tick().await;

                while audio_buffer.has_chunk() {
                    if let Some(chunk) = audio_buffer.try_read_chunk() {
                        state
                            .samples_captured
                            .fetch_add(chunk.samples.len() as u64, Ordering::SeqCst);
                        state.chunks_processed.fetch_add(1, Ordering::SeqCst);

                        if chunk_tx.send(chunk).await.is_err() {
                            break;
                        }
                    }
                }
            }

            for chunk in audio_buffer.drain() {
                let _ = chunk_tx.send(chunk).await;
            }
        })
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
        self.validate()?;
        let audio_config = self.resolve_device()?;

        let (chunk_tx, chunk_rx) = mpsc::channel(100);
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(1);

        let state = Arc::new(SessionState::new());

        let router_handle = self.start_router(chunk_rx, router_cmd_rx).await?;
        let (capture_stream, ring_consumer) = audio_config.device.start_capture()?;
        let capture_handle =
            self.spawn_capture_bridge(&audio_config, ring_consumer, chunk_tx, Arc::clone(&state));

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
        assert_eq!(builder.device, DeviceSelection::SystemDefault);
        assert!(builder.sinks.is_empty());
    }

    #[test]
    fn test_builder_device() {
        let builder = StreamAudio::builder().device("My Microphone");
        assert_eq!(
            builder.device,
            DeviceSelection::ByName("My Microphone".to_string())
        );
    }

    #[test]
    fn test_builder_device_default() {
        let builder = StreamAudio::builder()
            .device("Some Device")
            .device_default();
        assert_eq!(builder.device, DeviceSelection::SystemDefault);
    }

    #[test]
    fn test_builder_format() {
        let builder = StreamAudio::builder().format(FormatPreset::Native);
        assert_eq!(builder.format, FormatPreset::Native);
    }
}
