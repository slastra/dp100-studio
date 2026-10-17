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
