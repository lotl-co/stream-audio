//! Builder pattern for `StreamAudio`.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::pipeline::{spawn_capture_bridge, CaptureConfig, Router, SinkRoute};
use crate::session::{Session, SessionState};
use crate::sink::Sink;
use crate::source::{AudioDevice, SourceId};
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

/// Configuration for an audio source in multi-source mode.
#[derive(Debug, Clone)]
pub struct AudioSource {
    /// Device selection for this source.
    pub device: DeviceSelection,
}

impl AudioSource {
    /// Create a source from the system default input device.
    pub fn default_device() -> Self {
        Self {
            device: DeviceSelection::SystemDefault,
        }
    }

    /// Create a source from a specific device by name.
    pub fn device(name: impl Into<String>) -> Self {
        Self {
            device: DeviceSelection::ByName(name.into()),
        }
    }
}

/// Resolved audio configuration after opening the device.
struct ResolvedAudioConfig {
    device: AudioDevice,
    /// Target format (what sinks receive)
    target_sample_rate: u32,
    target_channels: u16,
    /// Device format (what CPAL actually captures)
    device_sample_rate: u32,
    device_channels: u16,
}

/// Builder for configuring and starting audio capture.
///
/// Use [`StreamAudio::builder()`] to create a new builder.
///
/// # Single-Source Example (v0.1 compatible)
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
/// # Multi-Source Example (v0.2)
///
/// ```ignore
/// use stream_audio::{StreamAudio, AudioSource, FileSink, ChannelSink, FormatPreset};
///
/// let session = StreamAudio::builder()
///     .add_source("mic", AudioSource::device("MacBook Pro Microphone"))
///     .add_source("speaker", AudioSource::device("BlackHole 2ch"))
///     .add_sink_from(FileSink::wav("mic.wav"), "mic")
///     .add_sink_from(FileSink::wav("speaker.wav"), "speaker")
///     .add_sink_merged(FileSink::wav("merged.wav"), ["mic", "speaker"])
///     .format(FormatPreset::Transcription)
///     .start()
///     .await?;
/// ```
///
/// [`StreamAudio::builder()`]: crate::StreamAudio::builder
#[must_use]
pub struct StreamAudioBuilder {
    /// Single-source device (backward compatibility).
    device: DeviceSelection,
    /// Multi-source configurations: `(source_id, source_config)` pairs.
    sources: Vec<(SourceId, AudioSource)>,
    /// Format preset for all sources.
    format: FormatPreset,
    /// Configured sinks.
    sinks: Vec<Arc<dyn Sink>>,
    /// Routing for each sink (parallel to sinks vec).
    sink_routes: Vec<SinkRoute>,
    /// Event callback.
    event_callback: Option<EventCallback>,
    /// Stream configuration.
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
            sources: Vec::new(),
            format: FormatPreset::default(),
            sinks: Vec::new(),
            sink_routes: Vec::new(),
            event_callback: None,
            config: StreamConfig::default(),
        }
    }

    // ========== Single-Source API (v0.1 compatible) ==========

    /// Use the system's default input device.
    ///
    /// This is the default behavior if no device is specified.
    /// For multi-source capture, use [`add_source()`](Self::add_source) instead.
    pub fn device_default(mut self) -> Self {
        self.device = DeviceSelection::SystemDefault;
        self
    }

    /// Use a specific input device by name.
    ///
    /// Use [`list_input_devices()`](crate::list_input_devices) to get available device names.
    /// For multi-source capture, use [`add_source()`](Self::add_source) instead.
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
    /// In single-source mode, the sink receives all audio.
    /// In multi-source mode with no routing specified, the sink receives from all sources.
    ///
    /// For routing to specific sources, use [`add_sink_from()`](Self::add_sink_from)
    /// or [`add_sink_merged()`](Self::add_sink_merged).
    pub fn add_sink<S: Sink + 'static>(mut self, sink: S) -> Self {
        self.sinks.push(Arc::new(sink));
        self.sink_routes.push(SinkRoute::All);
        self
    }

    // ========== Multi-Source API (v0.2) ==========

    /// Add an audio source with an identifier.
    ///
    /// Sources are identified by a string ID that can be used for routing
    /// sinks to specific sources.
    ///
    /// # Example
    ///
    /// ```ignore
    /// StreamAudio::builder()
    ///     .add_source("mic", AudioSource::device("MacBook Pro Microphone"))
    ///     .add_source("speaker", AudioSource::device("BlackHole 2ch"))
    ///     // ...
    /// ```
    pub fn add_source(mut self, id: impl Into<SourceId>, source: AudioSource) -> Self {
        self.sources.push((id.into(), source));
        self
    }

    /// Add a sink that receives audio from a specific source.
    ///
    /// # Example
    ///
    /// ```ignore
    /// StreamAudio::builder()
    ///     .add_source("mic", AudioSource::device("Microphone"))
    ///     .add_sink_from(FileSink::wav("mic.wav"), "mic")
    /// ```
    pub fn add_sink_from<S: Sink + 'static>(
        mut self,
        sink: S,
        source_id: impl Into<SourceId>,
    ) -> Self {
        self.sinks.push(Arc::new(sink));
        self.sink_routes.push(SinkRoute::Single(source_id.into()));
        self
    }

    /// Add a sink that receives merged audio from multiple sources.
    ///
    /// The merger combines audio from the specified sources by averaging samples.
    ///
    /// # Example
    ///
    /// ```ignore
    /// StreamAudio::builder()
    ///     .add_source("mic", AudioSource::device("Microphone"))
    ///     .add_source("speaker", AudioSource::device("BlackHole"))
    ///     .add_sink_merged(FileSink::wav("merged.wav"), ["mic", "speaker"])
    /// ```
    pub fn add_sink_merged<S, I, Id>(mut self, sink: S, source_ids: I) -> Self
    where
        S: Sink + 'static,
        I: IntoIterator<Item = Id>,
        Id: Into<SourceId>,
    {
        self.sinks.push(Arc::new(sink));
        self.sink_routes.push(SinkRoute::merged(source_ids));
        self
    }

    /// Returns true if this is a multi-source configuration.
    pub fn is_multi_source(&self) -> bool {
        !self.sources.is_empty()
    }

    /// Returns the configured source IDs.
    pub fn source_ids(&self) -> Vec<SourceId> {
        self.sources.iter().map(|(id, _)| id.clone()).collect()
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

        // Query the device's actual native format
        let (device_sample_rate, device_channels) = device.native_config()?;

        // Determine target format from preset (or use device native if Native preset)
        let target_sample_rate = self.format.sample_rate().unwrap_or(device_sample_rate);
        let target_channels = self.format.channels().unwrap_or(device_channels);

        Ok(ResolvedAudioConfig {
            device,
            target_sample_rate,
            target_channels,
            device_sample_rate,
            device_channels,
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

    /// Creates the capture configuration from resolved audio parameters.
    fn create_capture_config(
        &self,
        audio_config: &ResolvedAudioConfig,
        source_id: Option<SourceId>,
    ) -> CaptureConfig {
        CaptureConfig {
            device_sample_rate: audio_config.device_sample_rate,
            device_channels: audio_config.device_channels,
            target_sample_rate: audio_config.target_sample_rate,
            target_channels: audio_config.target_channels,
            chunk_duration: self.config.chunk_duration,
            source_id,
        }
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

        if self.is_multi_source() {
            self.start_multi_source().await
        } else {
            self.start_single_source().await
        }
    }

    /// Starts a single-source capture session (v0.1 compatible).
    async fn start_single_source(self) -> Result<Session, StreamAudioError> {
        let audio_config = self.resolve_device()?;

        let (chunk_tx, chunk_rx) = mpsc::channel(100);
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(1);

        let state = Arc::new(SessionState::new());

        let router_handle = self.start_router(chunk_rx, router_cmd_rx).await?;
        let (capture_stream, ring_consumer) = audio_config.device.start_capture()?;

        let capture_config = self.create_capture_config(&audio_config, None);
        let capture_handle =
            spawn_capture_bridge(ring_consumer, &capture_config, chunk_tx, Arc::clone(&state));

        Ok(Session::new(
            state,
            router_cmd_tx,
            router_handle,
            capture_handle,
            capture_stream,
        ))
    }

    /// Starts a multi-source capture session.
    async fn start_multi_source(self) -> Result<Session, StreamAudioError> {
        if self.sources.is_empty() {
            return Err(StreamAudioError::NoSourcesConfigured);
        }

        let (chunk_tx, chunk_rx) = mpsc::channel(100);
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(1);

        let state = Arc::new(SessionState::new());

        // Create router with routing
        let source_ids = self.source_ids();

        // Determine target sample rate and channels for merged audio.
        // For Native format, use common defaults (16kHz mono) since merger needs consistent values.
        let target_sample_rate = self.format.sample_rate().unwrap_or(16000);
        let target_channels = self.format.channels().unwrap_or(1);

        let mut router = Router::with_routing(
            self.sinks.clone(),
            &self.sink_routes,
            source_ids,
            self.config.clone(),
            target_sample_rate,
            target_channels,
        )?;
        if let Some(callback) = self.event_callback.clone() {
            router = router.with_event_callback(callback);
        }
        router.start_sinks().await?;

        let router_handle = tokio::spawn(async move {
            router.run(chunk_rx, router_cmd_rx).await;
        });

        // Start capture for each source
        let mut capture_handles = Vec::new();
        let mut capture_streams = Vec::new();

        for (source_id, source) in &self.sources {
            let audio_config = self.resolve_source_device(source)?;
            let (capture_stream, ring_consumer) = audio_config.device.start_capture()?;

            let capture_config = self.create_capture_config(&audio_config, Some(source_id.clone()));
            let capture_handle = spawn_capture_bridge(
                ring_consumer,
                &capture_config,
                chunk_tx.clone(),
                Arc::clone(&state),
            );

            capture_handles.push(capture_handle);
            capture_streams.push(capture_stream);
        }

        Ok(Session::new_multi(
            state,
            router_cmd_tx,
            router_handle,
            capture_handles,
            capture_streams,
        ))
    }

    /// Resolves a source's device.
    fn resolve_source_device(
        &self,
        source: &AudioSource,
    ) -> Result<ResolvedAudioConfig, StreamAudioError> {
        let device = match &source.device {
            DeviceSelection::SystemDefault => AudioDevice::open_default()?,
            DeviceSelection::ByName(name) => AudioDevice::open_by_name(name)?,
        };

        let (device_sample_rate, device_channels) = device.native_config()?;
        let target_sample_rate = self.format.sample_rate().unwrap_or(device_sample_rate);
        let target_channels = self.format.channels().unwrap_or(device_channels);

        Ok(ResolvedAudioConfig {
            device,
            target_sample_rate,
            target_channels,
            device_sample_rate,
            device_channels,
        })
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

    #[test]
    fn test_builder_add_source() {
        let builder = StreamAudio::builder()
            .add_source("mic", AudioSource::device("Microphone"))
            .add_source("speaker", AudioSource::device("BlackHole"));

        assert!(builder.is_multi_source());
        assert_eq!(builder.sources.len(), 2);
        assert_eq!(builder.source_ids().len(), 2);
    }

    #[test]
    fn test_builder_sink_routing() {
        let builder = StreamAudio::builder()
            .add_source("mic", AudioSource::default_device())
            .add_source("speaker", AudioSource::default_device())
            .add_sink_from(crate::sink::ChannelSink::new(mpsc::channel(1).0), "mic")
            .add_sink_merged(
                crate::sink::ChannelSink::new(mpsc::channel(1).0),
                ["mic", "speaker"],
            );

        assert_eq!(builder.sinks.len(), 2);
        assert_eq!(builder.sink_routes.len(), 2);
        assert!(matches!(&builder.sink_routes[0], SinkRoute::Single(_)));
        assert!(matches!(&builder.sink_routes[1], SinkRoute::Merged(_)));
    }

    #[test]
    fn test_builder_backward_compat() {
        // v0.1 style API should still work
        let builder = StreamAudio::builder()
            .device("Microphone")
            .add_sink(crate::sink::ChannelSink::new(mpsc::channel(1).0));

        assert!(!builder.is_multi_source());
        assert_eq!(builder.sinks.len(), 1);
        assert!(matches!(&builder.sink_routes[0], SinkRoute::All));
    }
}
