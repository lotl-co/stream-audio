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
        tracing::trace!(
            "FileSink {}: writing {} samples, ts={:?}",
            self.name,
            chunk.samples.len(),
            chunk.timestamp
        );

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

    #[tokio::test]
    async fn test_file_sink_invalid_path_error() {
        // Try to write to a nonexistent directory
        let path = PathBuf::from("/nonexistent/directory/test.wav");
        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap(); // on_start doesn't create file

        let chunk = AudioChunk::new(vec![100, 200], Duration::ZERO, 16000, 1);
        let result = sink.write(&chunk).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[tokio::test]
    async fn test_file_sink_flush_before_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        // Flush before any write should succeed (no-op)
        let result = sink.flush().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_file_sink_flush_after_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        let chunk = AudioChunk::new(vec![100, 200, 300], Duration::ZERO, 16000, 1);
        sink.write(&chunk).await.unwrap();

        // Flush should succeed and ensure data is written to disk
        let result = sink.flush().await;
        assert!(result.is_ok());

        // File should exist after flush
        assert!(path.exists());

        sink.on_stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_file_sink_on_stop_before_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        // Stop without writing anything - should not create file
        let result = sink.on_stop().await;
        assert!(result.is_ok());

        // File should NOT exist (no data was written)
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_file_sink_multiple_chunks_correct_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        // Write multiple chunks
        let chunk1 = AudioChunk::new(vec![100, 200], Duration::ZERO, 16000, 1);
        let chunk2 = AudioChunk::new(vec![300, 400], Duration::from_millis(100), 16000, 1);
        let chunk3 = AudioChunk::new(vec![500, 600], Duration::from_millis(200), 16000, 1);

        sink.write(&chunk1).await.unwrap();
        sink.write(&chunk2).await.unwrap();
        sink.write(&chunk3).await.unwrap();

        sink.on_stop().await.unwrap();

        // Read file and verify header has correct data size
        let data = std::fs::read(&path).unwrap();

        // Total samples: 6, each 2 bytes = 12 bytes of audio data
        // Data size is at offset 40 (little-endian u32)
        let data_size = u32::from_le_bytes([data[40], data[41], data[42], data[43]]);
        assert_eq!(data_size, 12); // 6 samples * 2 bytes

        // File size (at offset 4) should be header_size - 8 + data_size
        let file_size = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        assert_eq!(file_size, WAV_HEADER_SIZE as u32 - 8 + 12);
    }

    #[tokio::test]
    async fn test_file_sink_stereo_wav_header() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stereo.wav");

        let sink = FileSink::wav(&path);
        sink.on_start().await.unwrap();

        // Write stereo audio (interleaved L,R,L,R)
        let chunk = AudioChunk::new(vec![100, 200, 300, 400], Duration::ZERO, 44100, 2);
        sink.write(&chunk).await.unwrap();

        sink.on_stop().await.unwrap();

        let data = std::fs::read(&path).unwrap();

        // Channels at offset 22 (u16 LE)
        let channels = u16::from_le_bytes([data[22], data[23]]);
        assert_eq!(channels, 2);

        // Sample rate at offset 24 (u32 LE)
        let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        assert_eq!(sample_rate, 44100);

        // Byte rate at offset 28 (u32 LE) = sample_rate * channels * bytes_per_sample
        let byte_rate = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        assert_eq!(byte_rate, 44100 * 2 * 2); // 44100 Hz * 2 channels * 2 bytes

        // Block align at offset 32 (u16 LE) = channels * bytes_per_sample
        let block_align = u16::from_le_bytes([data[32], data[33]]);
        assert_eq!(block_align, 4); // 2 channels * 2 bytes
    }

    #[tokio::test]
    async fn test_file_sink_different_sample_rates() {
        let dir = tempdir().unwrap();

        // Test 48kHz
        let path_48k = dir.path().join("48k.wav");
        let sink_48k = FileSink::wav(&path_48k);
        sink_48k.on_start().await.unwrap();
        let chunk = AudioChunk::new(vec![100, 200], Duration::ZERO, 48000, 1);
        sink_48k.write(&chunk).await.unwrap();
        sink_48k.on_stop().await.unwrap();

        let data = std::fs::read(&path_48k).unwrap();
        let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        assert_eq!(sample_rate, 48000);

        // Test 8kHz
        let path_8k = dir.path().join("8k.wav");
        let sink_8k = FileSink::wav(&path_8k);
        sink_8k.on_start().await.unwrap();
        let chunk = AudioChunk::new(vec![100, 200], Duration::ZERO, 8000, 1);
        sink_8k.write(&chunk).await.unwrap();
        sink_8k.on_stop().await.unwrap();

        let data = std::fs::read(&path_8k).unwrap();
        let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        assert_eq!(sample_rate, 8000);
    }

    #[test]
    fn test_file_sink_name() {
        let sink = FileSink::wav("/path/to/audio.wav");
        assert_eq!(sink.name(), "file:/path/to/audio.wav");
    }
}
