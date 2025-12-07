//! WAV file sink implementation.

use crate::sink::Sink;
use crate::{AudioChunk, SinkError};
use async_trait::async_trait;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

// WAV file format constants
// See: http://soundfile.sapp.org/doc/WaveFormat/

/// Byte offset of the file size field in WAV header (RIFF chunk size).
const WAV_FILE_SIZE_OFFSET: u64 = 4;

/// Byte offset of the data chunk size field in WAV header.
const WAV_DATA_SIZE_OFFSET: u64 = 40;

/// Size of the WAV header in bytes (RIFF + fmt + data chunk headers).
const WAV_HEADER_SIZE: usize = 44;

/// Size of the fmt chunk data (16 bytes for PCM).
const WAV_FMT_CHUNK_SIZE: u32 = 16;

/// Audio format code for PCM (uncompressed).
const WAV_FORMAT_PCM: u16 = 1;

/// Bits per sample for 16-bit audio.
const WAV_BITS_PER_SAMPLE: u16 = 16;

/// Bytes per sample (16-bit = 2 bytes).
const BYTES_PER_SAMPLE: u64 = 2;

/// A sink that writes audio to a WAV file.
///
/// The file is created on first write and finalized (header updated)
/// on `on_stop()`. All file I/O is performed in a blocking thread pool
/// to avoid blocking the async runtime.
///
/// # Example
///
/// ```no_run
/// use stream_audio::FileSink;
///
/// let sink = FileSink::wav("recording.wav");
/// // Use with StreamAudio builder...
/// ```
pub struct FileSink {
    name: String,
    path: Arc<PathBuf>,
    state: Arc<Mutex<FileState>>,
}

struct FileState {
    writer: Option<BufWriter<File>>,
    samples_written: u64,
    sample_rate: u32,
    channels: u16,
}

impl FileSink {
    /// Creates a new file sink that writes WAV format.
    pub fn wav(path: impl AsRef<Path>) -> Self {
        Self {
            name: format!("file:{}", path.as_ref().display()),
            path: Arc::new(path.as_ref().to_path_buf()),
            state: Arc::new(Mutex::new(FileState {
                writer: None,
                samples_written: 0,
                sample_rate: 0,
                channels: 0,
            })),
        }
    }

    /// Flush buffered data to disk.
    ///
    /// This is useful for ensuring data is persisted during long recordings.
    /// Note: This does NOT update the WAV header - that happens on `on_stop()`.
    ///
    /// This operation runs in a blocking thread to avoid blocking the async runtime.
    pub async fn flush(&self) -> Result<(), SinkError> {
        let state = Arc::clone(&self.state);
        let path = Arc::clone(&self.path);

        tokio::task::spawn_blocking(move || {
            let mut state = state.blocking_lock();
            if let Some(ref mut writer) = state.writer {
                writer
                    .flush()
                    .map_err(|e| SinkError::file_error(&*path, e))?;
            }
            Ok(())
        })
        .await
        .map_err(|e| SinkError::custom(format!("flush task panicked: {e}")))?
    }

    /// Writes a complete WAV header with the given parameters.
    ///
    /// The header includes RIFF, fmt, and data chunk headers (44 bytes total).
    fn write_wav_header(
        writer: &mut BufWriter<File>,
        sample_rate: u32,
        channels: u16,
        data_size: u32,
    ) -> std::io::Result<()> {
        // RIFF container header
        writer.write_all(b"RIFF")?;
        let file_size = WAV_HEADER_SIZE as u32 - 8 + data_size; // Total size minus RIFF header
        writer.write_all(&file_size.to_le_bytes())?;
        writer.write_all(b"WAVE")?;

        // fmt subchunk (format specification)
        writer.write_all(b"fmt ")?;
        writer.write_all(&WAV_FMT_CHUNK_SIZE.to_le_bytes())?;
        writer.write_all(&WAV_FORMAT_PCM.to_le_bytes())?;
        writer.write_all(&channels.to_le_bytes())?;
        writer.write_all(&sample_rate.to_le_bytes())?;

        let bytes_per_sample = WAV_BITS_PER_SAMPLE / 8;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bytes_per_sample);
        writer.write_all(&byte_rate.to_le_bytes())?;

        let block_align = channels * bytes_per_sample;
        writer.write_all(&block_align.to_le_bytes())?;
        writer.write_all(&WAV_BITS_PER_SAMPLE.to_le_bytes())?;

        // data subchunk header
        writer.write_all(b"data")?;
        writer.write_all(&data_size.to_le_bytes())?;

        Ok(())
    }

    /// Updates the WAV header with the final data size after recording.
    ///
    /// This seeks back to update the file size and data size fields.
    fn update_wav_header(writer: &mut BufWriter<File>, data_size: u32) -> std::io::Result<()> {
        // Update RIFF chunk size (file size - 8)
        let file_size = WAV_HEADER_SIZE as u32 - 8 + data_size;
        writer.seek(SeekFrom::Start(WAV_FILE_SIZE_OFFSET))?;
        writer.write_all(&file_size.to_le_bytes())?;

        // Update data chunk size
        writer.seek(SeekFrom::Start(WAV_DATA_SIZE_OFFSET))?;
        writer.write_all(&data_size.to_le_bytes())?;

        // Seek to end for any further writes
        writer.seek(SeekFrom::End(0))?;

        Ok(())
    }

    /// Performs the actual write operation in a blocking context.
    fn write_samples_blocking(
        state: &mut FileState,
        path: &PathBuf,
        samples: &[i16],
        sample_rate: u32,
        channels: u16,
    ) -> Result<(), SinkError> {
        // Initialize on first write
        if state.writer.is_none() {
            let file = File::create(path).map_err(|e| SinkError::file_error(path, e))?;
            let mut writer = BufWriter::new(file);

            // Write placeholder header (will be updated on stop)
            Self::write_wav_header(&mut writer, sample_rate, channels, 0)
                .map_err(|e| SinkError::file_error(path, e))?;

            state.writer = Some(writer);
            state.sample_rate = sample_rate;
            state.channels = channels;
        }

        // Write samples
        if let Some(ref mut writer) = state.writer {
            for sample in samples {
                writer
                    .write_all(&sample.to_le_bytes())
                    .map_err(|e| SinkError::file_error(path, e))?;
            }
            state.samples_written += samples.len() as u64;
        }

        Ok(())
    }

    /// Finalizes the WAV file by updating the header with actual sizes.
    fn finalize_blocking(state: &mut FileState, path: &PathBuf) -> Result<(), SinkError> {
        let data_size = (state.samples_written * BYTES_PER_SAMPLE) as u32;

        if let Some(ref mut writer) = state.writer {
            Self::update_wav_header(writer, data_size)
                .map_err(|e| SinkError::file_error(path, e))?;

            writer.flush().map_err(|e| SinkError::file_error(path, e))?;
        }

        state.writer = None;
        Ok(())
    }
}

#[async_trait]
impl Sink for FileSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_start(&self) -> Result<(), SinkError> {
        // File will be created on first write when we know the format
        Ok(())
    }

    async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError> {
        // Clone Arc for the blocking task (cheap - just reference count increment)
        let samples = Arc::clone(&chunk.samples);
        let sample_rate = chunk.sample_rate;
        let channels = chunk.channels;
        let state = Arc::clone(&self.state);
        let path = Arc::clone(&self.path);

        // Run file I/O in blocking thread pool
        tokio::task::spawn_blocking(move || {
            let mut state = state.blocking_lock();
            Self::write_samples_blocking(&mut state, &path, &samples, sample_rate, channels)
        })
        .await
        .map_err(|e| SinkError::custom(format!("write task panicked: {e}")))?
    }

    async fn on_stop(&self) -> Result<(), SinkError> {
        let state = Arc::clone(&self.state);
        let path = Arc::clone(&self.path);

        // Run finalization in blocking thread pool
        tokio::task::spawn_blocking(move || {
            let mut state = state.blocking_lock();
            Self::finalize_blocking(&mut state, &path)
        })
        .await
        .map_err(|e| SinkError::custom(format!("finalize task panicked: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_sink_creates_wav() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        // Write some audio
        let chunk = AudioChunk::new(vec![100, 200, 300, 400], Duration::ZERO, 16000, 1);
        sink.write(&chunk).await.unwrap();

        sink.on_stop().await.unwrap();

        // Verify file exists and has valid WAV header
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], b"RIFF");
        assert_eq!(&data[8..12], b"WAVE");
        assert_eq!(&data[12..16], b"fmt ");
    }

    #[tokio::test]
    async fn test_file_sink_writes_samples() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        let samples = vec![0x1234i16, 0x5678i16];
        let chunk = AudioChunk::new(samples, Duration::ZERO, 16000, 1);
        sink.write(&chunk).await.unwrap();

        sink.on_stop().await.unwrap();

        // Read file and check data section
        let data = std::fs::read(&path).unwrap();
        // Data starts after header
        assert_eq!(data[WAV_HEADER_SIZE], 0x34); // Low byte of 0x1234
        assert_eq!(data[WAV_HEADER_SIZE + 1], 0x12); // High byte of 0x1234
        assert_eq!(data[WAV_HEADER_SIZE + 2], 0x78); // Low byte of 0x5678
        assert_eq!(data[WAV_HEADER_SIZE + 3], 0x56); // High byte of 0x5678
    }
}
