//! File sink implementation (placeholder - will be fleshed out).

use crate::{AudioChunk, SinkError};
use crate::sink::Sink;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::io::{BufWriter, Write};
use std::fs::File;

/// A sink that writes audio to a WAV file.
///
/// The file is created on `on_start()` and finalized (header updated)
/// on `on_stop()`.
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
    path: PathBuf,
    state: Mutex<FileState>,
}

struct FileState {
    writer: Option<BufWriter<File>>,
    samples_written: u64,
}

impl FileSink {
    /// Creates a new file sink that writes WAV format.
    pub fn wav(path: impl AsRef<Path>) -> Self {
        Self {
            name: format!("file:{}", path.as_ref().display()),
            path: path.as_ref().to_path_buf(),
            state: Mutex::new(FileState {
                writer: None,
                samples_written: 0,
            }),
        }
    }

    /// Flush buffered data to disk.
    ///
    /// This is useful for ensuring data is persisted during long recordings.
    /// Note: This does NOT update the WAV header - that happens on `on_stop()`.
    pub fn flush(&self) -> Result<(), SinkError> {
        let mut state = self.state.lock().unwrap();
        if let Some(ref mut writer) = state.writer {
            writer.flush().map_err(|e| SinkError::file_error(&self.path, e))?;
        }
        Ok(())
    }

    fn write_wav_header(writer: &mut BufWriter<File>, sample_rate: u32, channels: u16, data_size: u32) -> std::io::Result<()> {
        // RIFF header
        writer.write_all(b"RIFF")?;
        writer.write_all(&(36 + data_size).to_le_bytes())?; // File size - 8
        writer.write_all(b"WAVE")?;

        // fmt chunk
        writer.write_all(b"fmt ")?;
        writer.write_all(&16u32.to_le_bytes())?; // Chunk size
        writer.write_all(&1u16.to_le_bytes())?;  // Audio format (PCM)
        writer.write_all(&channels.to_le_bytes())?;
        writer.write_all(&sample_rate.to_le_bytes())?;
        let byte_rate = sample_rate * u32::from(channels) * 2; // 16-bit samples
        writer.write_all(&byte_rate.to_le_bytes())?;
        let block_align = channels * 2;
        writer.write_all(&block_align.to_le_bytes())?;
        writer.write_all(&16u16.to_le_bytes())?; // Bits per sample

        // data chunk header
        writer.write_all(b"data")?;
        writer.write_all(&data_size.to_le_bytes())?;

        Ok(())
    }

    fn update_wav_header(writer: &mut BufWriter<File>, data_size: u32) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};

        // Update RIFF chunk size
        writer.seek(SeekFrom::Start(4))?;
        writer.write_all(&(36 + data_size).to_le_bytes())?;

        // Update data chunk size
        writer.seek(SeekFrom::Start(40))?;
        writer.write_all(&data_size.to_le_bytes())?;

        // Seek to end
        writer.seek(SeekFrom::End(0))?;

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
        let mut state = self.state.lock().unwrap();

        // Initialize on first write
        if state.writer.is_none() {
            let file = File::create(&self.path)
                .map_err(|e| SinkError::file_error(&self.path, e))?;
            let mut writer = BufWriter::new(file);

            // Write placeholder header (will be updated on stop)
            Self::write_wav_header(&mut writer, chunk.sample_rate, chunk.channels, 0)
                .map_err(|e| SinkError::file_error(&self.path, e))?;

            state.writer = Some(writer);
        }

        // Write samples
        if let Some(ref mut writer) = state.writer {
            for sample in &chunk.samples {
                writer.write_all(&sample.to_le_bytes())
                    .map_err(|e| SinkError::file_error(&self.path, e))?;
            }
            state.samples_written += chunk.samples.len() as u64;
        }

        Ok(())
    }

    async fn on_stop(&self) -> Result<(), SinkError> {
        let mut state = self.state.lock().unwrap();

        // Calculate data size before borrowing writer
        let data_size = (state.samples_written * 2) as u32;

        if let Some(ref mut writer) = state.writer {
            // Update header with final size
            Self::update_wav_header(writer, data_size)
                .map_err(|e| SinkError::file_error(&self.path, e))?;

            // Flush
            writer.flush()
                .map_err(|e| SinkError::file_error(&self.path, e))?;
        }

        state.writer = None;
        Ok(())
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
        // WAV header is 44 bytes, then data
        assert_eq!(data[44], 0x34); // Low byte of 0x1234
        assert_eq!(data[45], 0x12); // High byte of 0x1234
        assert_eq!(data[46], 0x78); // Low byte of 0x5678
        assert_eq!(data[47], 0x56); // High byte of 0x5678
    }
}
