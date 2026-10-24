//! Cumulative wall times, not throughput promises: parallel stages may overlap.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

#[derive(Default)]
pub(super) struct Metrics {
    pub hash: AtomicU64,
    pub write: AtomicU64,
    pub sync: AtomicU64,
    pub recover: AtomicU64,
    pub publish: AtomicU64,
}
pub(super) struct Timer<'a> {
    start: Instant,
    counter: &'a AtomicU64,
}
pub(super) fn measure(counter: &AtomicU64) -> Timer<'_> {
    Timer {
        start: Instant::now(),
        counter,
    }
}
impl Drop for Timer<'_> {
    fn drop(&mut self) {
        self.counter.fetch_add(
            self.start.elapsed().as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
}
impl Metrics {
    pub fn report(&self, task: &str) {
        bevy::log::info!(
            task,
            hash_ms = self.hash.load(Ordering::Relaxed) / 1000,
            write_ms = self.write.load(Ordering::Relaxed) / 1000,
            sync_ms = self.sync.load(Ordering::Relaxed) / 1000,
            recovery_ms = self.recover.load(Ordering::Relaxed) / 1000,
            publication_ms = self.publish.load(Ordering::Relaxed) / 1000,
            "Transfer stage timings (cumulative)"
        );
    }
}
