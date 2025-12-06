//! Custom sink example.
//!
//! Demonstrates how to implement the Sink trait for custom audio destinations.
//!
//! Run with: cargo run --example custom_sink

use std::sync::atomic::{AtomicI16, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use stream_audio::{AudioChunk, FormatPreset, Sink, SinkError, StreamAudio};

/// A custom sink that computes audio statistics in real-time.
struct StatsSink {
    name: String,
    sample_count: AtomicU64,
    min_sample: AtomicI16,
    max_sample: AtomicI16,
}

impl StatsSink {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            sample_count: AtomicU64::new(0),
            min_sample: AtomicI16::new(i16::MAX),
            max_sample: AtomicI16::new(i16::MIN),
        }
    }

    fn stats(&self) -> (u64, i16, i16) {
        (
            self.sample_count.load(Ordering::Relaxed),
            self.min_sample.load(Ordering::Relaxed),
            self.max_sample.load(Ordering::Relaxed),
        )
    }
}

#[async_trait]
impl Sink for StatsSink {
    fn name(&self) -> &str {
        &self.name
    }

    async fn on_start(&self) -> Result<(), SinkError> {
        println!("[{}] Starting...", self.name);
        Ok(())
    }

    async fn write(&self, chunk: &AudioChunk) -> Result<(), SinkError> {
        // Update sample count
        self.sample_count
            .fetch_add(chunk.samples.len() as u64, Ordering::Relaxed);

        // Update min/max (approximate - may miss updates under contention)
        for &sample in chunk.samples.iter() {
            // Update minimum
            let mut current_min = self.min_sample.load(Ordering::Relaxed);
            while sample < current_min {
                match self.min_sample.compare_exchange_weak(
                    current_min,
                    sample,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(x) => current_min = x,
                }
            }

            // Update maximum
            let mut current_max = self.max_sample.load(Ordering::Relaxed);
            while sample > current_max {
                match self.max_sample.compare_exchange_weak(
                    current_max,
                    sample,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(x) => current_max = x,
                }
            }
        }

        Ok(())
    }

    async fn on_stop(&self) -> Result<(), SinkError> {
        let (count, min, max) = self.stats();
        println!("[{}] Stopping. Final stats:", self.name);
        println!("  Samples processed: {}", count);
        println!("  Min amplitude: {}", min);
        println!("  Max amplitude: {}", max);
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("Recording with custom stats sink for 3 seconds...");

    let stats_sink = StatsSink::new("audio-stats");

    let session = StreamAudio::builder()
        .format(FormatPreset::Transcription)
        .add_sink(stats_sink)
        .start()
        .await?;

    // Record for 3 seconds, showing periodic updates
    for i in 1..=3 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("{}s elapsed...", i);
    }

    session.stop().await?;

    println!("\nDone!");

    Ok(())
}
