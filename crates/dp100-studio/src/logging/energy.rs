//! Energy and charge delivered, integrated from the measurements.

use std::time::{Duration, Instant};

/// Integrates power (to mWh) and current (to mAh) with the trapezoid rule
/// over the measured time between samples. A gap much longer than the
/// sampling interval (a disconnect, a stalled poll) is not bridged: nobody
/// knows what flowed in it. Gaps are counted instead.
#[derive(Debug, Clone)]
pub struct Energy {
    last: Option<(Instant, f64, f64)>,
    mwh: f64,
    mah: f64,
    gaps: u32,
    /// Time covered by integrated samples.
    covered: Duration,
    /// Spacing beyond which two samples count as a gap.
    max_step: Duration,
}

impl Energy {
    /// `interval` is the nominal time between samples.
    pub fn new(interval: Duration) -> Self {
        Self {
            last: None,
            mwh: 0.0,
            mah: 0.0,
            gaps: 0,
            covered: Duration::ZERO,
            max_step: interval * 5,
        }
    }

    pub fn set_interval(&mut self, interval: Duration) {
        self.max_step = interval * 5;
    }

    /// Adds a sample of `mv` millivolts and `ma` milliamps taken at `at`.
    pub fn add(&mut self, at: Instant, mv: u16, ma: u16) {
        let v = f64::from(mv) / 1000.0;
        let i = f64::from(ma);
        if let Some((t0, v0, i0)) = self.last {
            let dt = at.saturating_duration_since(t0);
            if dt > self.max_step {
                self.gaps += 1;
            } else if !dt.is_zero() {
                let h = dt.as_secs_f64() / 3600.0;
                // P = V·I in mW, so ∫P dt in mWh.
                self.mwh += (v0 * i0 + v * i) / 2.0 * h;
                self.mah += (i0 + i) / 2.0 * h;
                self.covered += dt;
            }
        }
        self.last = Some((at, v, i));
    }

    pub fn mwh(&self) -> f64 {
        self.mwh
    }

    pub fn mah(&self) -> f64 {
        self.mah
    }

    pub fn gaps(&self) -> u32 {
        self.gaps
    }

    pub fn covered(&self) -> Duration {
        self.covered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(50);

    #[test]
    fn constant_load_for_an_hour() {
        // 5 V at 100 mA for one hour: 500 mWh and 100 mAh.
        let mut e = Energy::new(TICK);
        let t0 = Instant::now();
        for k in 0..=72_000u32 {
            e.add(t0 + TICK * k, 5_000, 100);
        }
        assert!((e.mwh() - 500.0).abs() < 1e-6, "{}", e.mwh());
        assert!((e.mah() - 100.0).abs() < 1e-6, "{}", e.mah());
        assert_eq!(e.covered(), Duration::from_secs(3600));
    }

    #[test]
    fn a_ramp_integrates_to_its_average() {
        // Current ramps 0 to 200 mA over 10 s at 1 V: average 100 mA.
        let mut e = Energy::new(TICK);
        let t0 = Instant::now();
        for k in 0..=200u16 {
            e.add(t0 + TICK * u32::from(k), 1_000, k);
        }
        let want_mah = 100.0 * 10.0 / 3600.0;
        assert!((e.mah() - want_mah).abs() < 1e-9);
    }

    #[test]
    fn gaps_are_skipped_and_counted() {
        let mut e = Energy::new(TICK);
        let t0 = Instant::now();
        e.add(t0, 5_000, 100);
        e.add(t0 + TICK, 5_000, 100);
        // Three seconds with no samples.
        e.add(t0 + TICK + Duration::from_secs(3), 5_000, 100);
        e.add(t0 + TICK * 2 + Duration::from_secs(3), 5_000, 100);
        assert_eq!(e.gaps(), 1);
        assert_eq!(e.covered(), TICK * 2);
    }
}
