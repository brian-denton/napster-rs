//! Bandwidth throttling shared by every transfer in one direction, the way
//! a modem's bandwidth was shared: two downloads on a 56K line each got
//! half of it.

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// How much unused bandwidth may be saved up, in seconds of transfer.
const BURST_SECS: f64 = 0.25;

#[derive(Debug)]
pub struct Throttle {
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Default for Throttle {
    fn default() -> Self {
        Self { bucket: Mutex::new(Bucket { tokens: 0.0, last: Instant::now() }) }
    }
}

impl Throttle {
    /// Account for `bytes` moved at no more than `rate` bytes/second
    /// (0 = unlimited), sleeping as needed. Callers share the rate.
    pub async fn take(&self, bytes: usize, rate: u64) {
        if rate == 0 || bytes == 0 {
            return;
        }
        let rate = rate as f64;
        let deficit = {
            let mut b = self.bucket.lock().await;
            let now = Instant::now();
            let burst = (rate * BURST_SECS).max(1.0);
            b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * rate).min(burst);
            b.last = now;
            b.tokens -= bytes as f64;
            -b.tokens
        };
        if deficit > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(deficit / rate)).await;
        }
    }
}

/// Read/write in slices of about 1/8 s at `rate` so progress stays smooth
/// on slow links.
pub fn chunk_size(rate: u64, max: usize) -> usize {
    if rate == 0 {
        max
    } else {
        ((rate / 8) as usize).clamp(256, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enforces_shared_rate() {
        let t = std::sync::Arc::new(Throttle::default());
        let start = Instant::now();
        // two "transfers" of 50 KB each at a shared 200 KB/s = ~0.5 s
        let mut tasks = vec![];
        for _ in 0..2 {
            let t = t.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..10 {
                    t.take(5_000, 200_000).await;
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let secs = start.elapsed().as_secs_f64();
        assert!(secs > 0.4 && secs < 1.5, "took {secs}s");
    }

    #[tokio::test]
    async fn unlimited_is_instant() {
        let t = Throttle::default();
        let start = Instant::now();
        t.take(10_000_000, 0).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn chunks() {
        assert_eq!(chunk_size(0, 65536), 65536);
        assert_eq!(chunk_size(5 * 1024, 65536), 640);
        assert_eq!(chunk_size(100, 65536), 256);
    }
}
