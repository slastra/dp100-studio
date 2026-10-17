use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Time source for [`crate::Device`]. The device's firmware needs pauses
/// between some writes; tests and dry runs swap in a clock that doesn't
/// actually wait.
pub trait Clock: Send {
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration);
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// A clock that only moves when slept on. Clones share one timeline, so a
/// test can hand one to a [`crate::Device`] and read the elapsed time from
/// another.
#[derive(Debug, Clone)]
pub struct ManualClock {
    origin: Instant,
    elapsed_ns: Arc<AtomicU64>,
}

impl Default for ManualClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            elapsed_ns: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.elapsed_ns.load(Ordering::SeqCst))
    }

    pub fn advance(&self, d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.elapsed_ns.fetch_add(ns, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.origin + self.elapsed()
    }

    fn sleep(&self, d: Duration) {
        self.advance(d);
    }
}
