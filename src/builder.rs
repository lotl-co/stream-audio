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

/// Channel capacity for audio chunks flowing to the router.
/// Large enough to buffer ~10 seconds at 100ms chunks.
const CHUNK_CHANNEL_CAPACITY: usize = 100;

/// Channel capacity for router commands.
/// Only need 1 since commands are rare (just Stop).
const COMMAND_CHANNEL_CAPACITY: usize = 1;

/// Specifies which audio input device to use.
#[derive(Debug, Clone, Default)]
pub(crate) enum DeviceSelection {
    /// Use the system's default input device.
    #[default]
    SystemDefault,
    /// Use a specific device by name.
    ByName(String),
    /// Capture system audio output via ScreenCaptureKit.
    /// Requires the `sck-native` feature on macOS.
    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    SystemAudio,
}

/// Configuration for an audio source in multi-source mode.
#[derive(Debug, Clone)]
pub struct AudioSource {
    /// Device selection for this source.
    pub(crate) device: DeviceSelection,
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

    /// Create a source that captures system audio (speaker output).
    ///
    /// This captures what you hear through your speakers/headphones,
    /// without requiring virtual audio devices like `BlackHole`.
    ///
    /// Requires the `sck-native` feature on macOS 13+ and Screen Recording permission.
    ///
    /// # Example
    ///
    /// ```ignore
    /// StreamAudio::builder()
    ///     .add_source("mic", AudioSource::default_device())
    ///     .add_source("speaker", AudioSource::system_audio())
    ///     .add_sink_from(FileSink::wav("mic.wav"), "mic")
    ///     .add_sink_from(FileSink::wav("speaker.wav"), "speaker")
    ///     .start()
    ///     .await?;
    /// ```
    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    pub fn system_audio() -> Self {
        Self {
            device: DeviceSelection::SystemAudio,
        }
    }
}

/// Resolved audio source - either a device or system audio backend.
enum ResolvedSource {
    /// CPAL audio device.
    Device(AudioDevice),
    /// System audio backend (macOS only with sck-native feature).
    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    SystemAudio(Box<dyn crate::source::system_audio::SystemAudioBackend>),
}

impl ResolvedSource {
    /// Start capture and return the stream + ring buffer consumer.
    /// Consumes self for system audio backends (they move into the `CaptureStream`).
    fn start_capture(
        self,
    ) -> Result<(crate::source::CaptureStream, ringbuf::HeapCons<i16>), StreamAudioError> {
        match self {
            ResolvedSource::Device(device) => device.start_capture(),
            #[cfg(all(target_os = "macos", feature = "sck-native"))]
            ResolvedSource::SystemAudio(backend) => backend.start_capture(),
        }
    }

    /// Get native sample rate and channel count.
    fn native_config(&self) -> Result<(u32, u16), StreamAudioError> {
        match self {
            ResolvedSource::Device(device) => device.native_config(),
            #[cfg(all(target_os = "macos", feature = "sck-native"))]
            ResolvedSource::SystemAudio(backend) => Ok(backend.native_config()),
        }
    }
}

/// Resolved audio configuration after opening the source.
struct ResolvedAudioConfig {
    source: ResolvedSource,
    /// Target format (what sinks receive)
    target_sample_rate: u32,
    target_channels: u16,
    /// Source format (what the source actually captures)
    device_sample_rate: u32,
    device_channels: u16,
}

/// Builder for configuring and starting audio capture.
///
/// Use [`StreamAudio::builder()`] to create a new builder.
///
/// # Single-Source Example
///
/// ```ignore
/// use stream_audio::{StreamAudio, AudioSource, FileSink, ChannelSink, FormatPreset};
/// use tokio::sync::mpsc;
///
/// let (tx, rx) = mpsc::channel(32);
///
/// let session = StreamAudio::builder()
///     .add_source("default", AudioSource::default_device())
///     .format(FormatPreset::Transcription)
///     .add_sink(FileSink::wav("output.wav"))
///     .add_sink(ChannelSink::new(tx))
///     .start()
///     .await?;
/// ```
///
/// # Multi-Source Example
///
/// ```ignore
/// use stream_audio::{StreamAudio, AudioSource, FileSink, ChannelSink, FormatPreset};
///
/// let session = StreamAudio::builder()
///     .add_source("mic", AudioSource::device("MacBook Pro Microphone"))
///     .add_source("speaker", AudioSource::device("BlackHole 2ch"))
///     .add_sink_from(FileSink::wav("mic.wav"), "mic")
///     .add_sink_from(FileSink::wav("speaker.wav"), "speaker")
///     .format(FormatPreset::Transcription)
///     .start()
///     .await?;
/// ```
///
/// [`StreamAudio::builder()`]: crate::StreamAudio::builder
#[must_use]
pub struct StreamAudioBuilder {
    /// Source configurations: `(source_id, source_config)` pairs.
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
            sources: Vec::new(),
            format: FormatPreset::default(),
            sinks: Vec::new(),
            sink_routes: Vec::new(),
            event_callback: None,
            config: StreamConfig::default(),
        }
    }

    /// Set the audio format preset.
    ///
    /// Default: [`FormatPreset::Transcription`] (16kHz mono)
    pub fn format(mut self, format: FormatPreset) -> Self {
        self.format = format;
        self
    }

    /// Add a sink to receive audio from all sources.
    ///
    /// For routing to a specific source, use [`add_sink_from()`](Self::add_sink_from).
    pub fn add_sink<S: Sink + 'static>(mut self, sink: S) -> Self {
        self.sinks.push(Arc::new(sink));
        self.sink_routes.push(SinkRoute::Broadcast);
        self
    }

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
        if self.sources.is_empty() {
            return Err(StreamAudioError::NoSourcesConfigured);
        }
        if self.sinks.is_empty() {
            return Err(StreamAudioError::NoSinksConfigured);
        }

        // Check for duplicate source IDs
        let mut seen = std::collections::HashSet::new();
        for (id, _) in &self.sources {
            if !seen.insert(id) {
                return Err(StreamAudioError::DuplicateSourceId {
                    source_id: id.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Creates the capture configuration from resolved audio parameters.
    fn create_capture_config(
        &self,
        audio_config: &ResolvedAudioConfig,
        source_id: SourceId,
        session_start: std::time::Instant,
    ) -> CaptureConfig {
        CaptureConfig {
            device_sample_rate: audio_config.device_sample_rate,
            device_channels: audio_config.device_channels,
            target_sample_rate: audio_config.target_sample_rate,
            target_channels: audio_config.target_channels,
            chunk_duration: self.config.chunk_duration,
            source_id: Some(source_id),
            session_start,
        }
    }

    /// Start audio capture.
    ///
    /// Returns a [`Session`] handle to control the capture.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No sources are configured
    /// - No sinks are configured
    /// - The audio device cannot be opened
    /// - Any sink fails to start
    pub async fn start(self) -> Result<Session, StreamAudioError> {
        self.validate()?;

        // Shared session epoch for synchronized timestamps across all sources
        let session_start = std::time::Instant::now();

        let (chunk_tx, chunk_rx) = mpsc::channel(CHUNK_CHANNEL_CAPACITY);
        let (router_cmd_tx, router_cmd_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);

        let state = Arc::new(SessionState::new());

        // Create router with routing
        let source_ids = self.source_ids();

        let mut router = Router::with_routing(
            self.sinks.clone(),
            &self.sink_routes,
            &source_ids,
            self.config.clone(),
        )?;
        if let Some(callback) = self.event_callback.clone() {
            router = router.with_event_callback(callback);
        }
        router.start_sinks().await?;

        let router_handle = tokio::spawn(async move {
            router.run(chunk_rx, router_cmd_rx).await;
        });

        // Separate sources: start non-system-audio sources first, then system audio.
        // This handles Bluetooth profile switching: when mic starts, Bluetooth may
        // switch from A2DP (high quality output) to HFP (low quality bidirectional).
        // By starting system audio LAST, we capture at the actual (possibly degraded) config.
        let (regular_sources, system_audio_sources): (Vec<_>, Vec<_>) = self
            .sources
            .iter()
            .partition(|(_, source)| !Self::is_system_audio_source(source));

        let mut capture_handles = Vec::new();
        let mut capture_streams = Vec::new();

        // Snapshot output device config before starting any sources
        #[cfg(all(target_os = "macos", feature = "sck-native"))]
        let initial_output_config = Self::query_output_device_config();

        // Start regular sources first (mic, named devices, etc.)
        for (source_id, source) in &regular_sources {
            self.start_single_source(
                source_id,
                source,
                &chunk_tx,
                &state,
                &mut capture_handles,
                &mut capture_streams,
                session_start,
            )?;
        }

        // Check if output config changed after starting regular sources
        #[cfg(all(target_os = "macos", feature = "sck-native"))]
        let post_mic_output_config = Self::query_output_device_config();

        // Start system audio sources last (they'll use the current config)
        for (source_id, source) in &system_audio_sources {
            // Emit config change warning if Bluetooth profile switched
            #[cfg(all(target_os = "macos", feature = "sck-native"))]
            if let (Some(initial), Some(current)) = (
                initial_output_config.as_ref(),
                post_mic_output_config.as_ref(),
            ) {
                if initial != current {
                    if let Some(ref callback) = self.event_callback {
                        callback(StreamEvent::AudioConfigChanged {
                            source_id: source_id.clone(),
                            previous: *initial,
                            current: *current,
                            message: format!(
                                "Output device config changed from {}Hz/{}ch to {}Hz/{}ch. \
                                 This may indicate Bluetooth profile switching (A2DP to HFP). \
                                 System audio will capture at the new config.",
                                initial.0, initial.1, current.0, current.1
                            ),
                        });
                    }
                }
            }

            self.start_single_source(
                source_id,
                source,
                &chunk_tx,
                &state,
                &mut capture_handles,
                &mut capture_streams,
                session_start,
            )?;
        }

        Ok(Session::new(
            state,
            router_cmd_tx,
            router_handle,
            capture_handles,
            capture_streams,
            self.source_ids(),
            self.event_callback.clone(),
        ))
    }

    /// Resolves a source's device or system audio backend.
    fn resolve_source_device(
        &self,
        source: &AudioSource,
    ) -> Result<ResolvedAudioConfig, StreamAudioError> {
        let resolved_source = match &source.device {
            DeviceSelection::SystemDefault => ResolvedSource::Device(AudioDevice::open_default()?),
            DeviceSelection::ByName(name) => {
                ResolvedSource::Device(AudioDevice::open_by_name(name)?)
            }
            #[cfg(all(target_os = "macos", feature = "sck-native"))]
            DeviceSelection::SystemAudio => {
                let backend = crate::source::system_audio::create_system_audio_backend()?;
                ResolvedSource::SystemAudio(backend)
            }
        };

        let (device_sample_rate, device_channels) = resolved_source.native_config()?;
        let target_sample_rate = self.format.sample_rate().unwrap_or(device_sample_rate);
        let target_channels = self.format.channels().unwrap_or(device_channels);

        Ok(ResolvedAudioConfig {
            source: resolved_source,
            target_sample_rate,
            target_channels,
            device_sample_rate,
            device_channels,
        })
    }

    /// Starts a single source and adds it to the capture handles/streams.
    #[allow(clippy::too_many_arguments)]
    fn start_single_source(
        &self,
        source_id: &SourceId,
        source: &AudioSource,
        chunk_tx: &mpsc::Sender<crate::chunk::AudioChunk>,
        state: &Arc<SessionState>,
        capture_handles: &mut Vec<tokio::task::JoinHandle<()>>,
        capture_streams: &mut Vec<crate::source::CaptureStream>,
        session_start: std::time::Instant,
    ) -> Result<(), StreamAudioError> {
        let audio_config = self.resolve_source_device(source)?;
        let capture_config =
            self.create_capture_config(&audio_config, source_id.clone(), session_start);
        let (capture_stream, ring_consumer) = audio_config.source.start_capture()?;
        let capture_handle = spawn_capture_bridge(
            ring_consumer,
            &capture_config,
            chunk_tx.clone(),
            Arc::clone(state),
            self.event_callback.clone(),
        );

        // Emit SourceStarted event
        if let Some(ref callback) = self.event_callback {
            callback(StreamEvent::SourceStarted {
                source_id: source_id.clone(),
            });
        }

        capture_handles.push(capture_handle);
        capture_streams.push(capture_stream);
        Ok(())
    }

    /// Queries the current default output device configuration.
    /// Returns None if query fails (no output device, etc.)
    #[cfg(all(target_os = "macos", feature = "sck-native"))]
    fn query_output_device_config() -> Option<(u32, u16)> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let config = device.default_output_config().ok()?;
        Some((config.sample_rate().0, config.channels()))
    }

    /// Checks if a source is a system audio source.
    fn is_system_audio_source(source: &AudioSource) -> bool {
        #[cfg(all(target_os = "macos", feature = "sck-native"))]
        if matches!(source.device, DeviceSelection::SystemAudio) {
            return true;
        }
        let _ = source; // suppress unused warning when feature not enabled
        false
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
        assert!(builder.sources.is_empty());
        assert!(builder.sinks.is_empty());
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

        assert_eq!(builder.sources.len(), 2);
        assert_eq!(builder.source_ids().len(), 2);
    }

    #[test]
    fn test_builder_sink_routing() {
        let builder = StreamAudio::builder()
            .add_source("mic", AudioSource::default_device())
            .add_source("speaker", AudioSource::default_device())
            .add_sink_from(crate::sink::ChannelSink::new(mpsc::channel(1).0), "mic")
            .add_sink_from(crate::sink::ChannelSink::new(mpsc::channel(1).0), "speaker");

        assert_eq!(builder.sinks.len(), 2);
        assert_eq!(builder.sink_routes.len(), 2);
        assert!(matches!(&builder.sink_routes[0], SinkRoute::Single(_)));
        assert!(matches!(&builder.sink_routes[1], SinkRoute::Single(_)));
    }

    #[test]
    fn test_builder_broadcast_sink() {
        let builder = StreamAudio::builder()
            .add_source("default", AudioSource::default_device())
            .add_sink(crate::sink::ChannelSink::new(mpsc::channel(1).0));

        assert_eq!(builder.sinks.len(), 1);
        assert!(matches!(&builder.sink_routes[0], SinkRoute::Broadcast));
    }

    #[test]
    fn test_builder_rejects_duplicate_source_ids() {
        let builder = StreamAudio::builder()
            .add_source("mic", AudioSource::default_device())
            .add_source("mic", AudioSource::default_device()) // Duplicate!
            .add_sink(crate::sink::ChannelSink::new(mpsc::channel(1).0));

        let result = builder.validate();
        assert!(matches!(
            result,
            Err(StreamAudioError::DuplicateSourceId { .. })
        ));
    }

    #[test]
    fn test_builder_rejects_no_sources() {
        let builder =
            StreamAudio::builder().add_sink(crate::sink::ChannelSink::new(mpsc::channel(1).0));

        let result = builder.validate();
        assert!(matches!(result, Err(StreamAudioError::NoSourcesConfigured)));
    }
}
