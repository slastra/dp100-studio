//! Measurement history for the graph, at several resolutions.
//!
//! Raw samples are kept for the whole history, and alongside them 0.1 s and
//! 1 s buckets holding each channel's minimum, maximum and last value. A view
//! reads from the coarsest resolution that is still finer than one pixel, so
//! drawing an hour costs no more than drawing a minute: a few thousand items
//! rather than 360,000 raw samples. Min and max survive aggregation, so a
//! single-sample spike stays visible at every zoom.

use std::collections::VecDeque;

/// Channels, in [`crate::ui::graph::Lane::ALL`] order.
pub const CHANNELS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    /// Time of the first sample in the bucket, seconds.
    pub t: f64,
    pub lo: [f32; CHANNELS],
    pub hi: [f32; CHANNELS],
    pub last: [f32; CHANNELS],
}

impl Bucket {
    fn of(t: f64, v: [f32; CHANNELS]) -> Self {
        Self {
            t,
            lo: v,
            hi: v,
            last: v,
        }
    }

    fn merge(&mut self, v: [f32; CHANNELS]) {
        for (k, x) in v.into_iter().enumerate() {
            self.lo[k] = self.lo[k].min(x);
            self.hi[k] = self.hi[k].max(x);
        }
        self.last = v;
    }
}

struct Tier {
    /// Bucket width in seconds; 0 for raw samples.
    res: f64,
    buckets: VecDeque<Bucket>,
}

impl Tier {
    fn push(&mut self, t: f64, v: [f32; CHANNELS]) {
        if self.res > 0.0
            && let Some(b) = self.buckets.back_mut()
            && (b.t / self.res).floor() == (t / self.res).floor()
        {
            b.merge(v);
            return;
        }
        self.buckets.push_back(Bucket::of(t, v));
    }

    fn trim(&mut self, before: f64) {
        while self.buckets.front().is_some_and(|b| b.t < before) {
            self.buckets.pop_front();
        }
    }
}

pub struct History {
    keep: f64,
    tiers: [Tier; 3],
}

/// The items a view draws, and the spacing beyond which two consecutive
/// items are drawn with a gap between them.
pub struct View<'a> {
    pub items: std::collections::vec_deque::Iter<'a, Bucket>,
    pub gap: f64,
}

impl History {
    /// Keeps `keep` seconds of history. Samples further apart than
    /// `gap` seconds are shown as a break in the line.
    pub fn new(keep: f64) -> Self {
        let tier = |res| Tier {
            res,
            buckets: VecDeque::new(),
        };
        Self {
            keep,
            tiers: [tier(0.0), tier(0.1), tier(1.0)],
        }
    }

    pub fn push(&mut self, t: f64, v: [f32; CHANNELS]) {
        for tier in &mut self.tiers {
            tier.push(t, v);
            tier.trim(t - self.keep);
        }
    }

    pub fn len(&self) -> usize {
        self.tiers[0].buckets.len()
    }

    pub fn latest(&self) -> Option<&Bucket> {
        self.tiers[0].buckets.back()
    }

    /// Everything from `start` on, at the coarsest resolution no wider than
    /// `secs_per_px`.
    pub fn view(&self, start: f64, secs_per_px: f64, gap: f64) -> View<'_> {
        let tier = self
            .tiers
            .iter()
            .rev()
            .find(|t| t.res <= secs_per_px)
            .unwrap_or(&self.tiers[0]);
        let b = &tier.buckets;
        // A bucket that began before `start` may still hold samples after it.
        let first = b.partition_point(|x| x.t + tier.res < start);
        View {
            items: b.range(first..),
            gap: gap.max(tier.res * 2.5),
        }
    }

    /// The raw sample nearest to `t`.
    pub fn nearest(&self, t: f64) -> Option<Bucket> {
        let b = &self.tiers[0].buckets;
        let i = b.partition_point(|x| x.t < t);
        [i.checked_sub(1), Some(i)]
            .into_iter()
            .flatten()
            .filter_map(|i| b.get(i))
            .min_by(|a, c| (a.t - t).abs().total_cmp(&(c.t - t).abs()))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(secs: u32, hz: u32) -> History {
        let mut h = History::new(3600.0);
        for k in 0..secs * hz {
            let t = f64::from(k) / f64::from(hz);
            // A single spike at t = 10 s.
            let v = if k == 10 * hz { 9.0 } else { 1.0 };
            h.push(t, [v, 0.0, 0.0, 0.0]);
        }
        h
    }

    #[test]
    fn picks_the_coarsest_tier_finer_than_a_pixel() {
        let h = filled(3600, 100);
        assert_eq!(h.view(0.0, 0.01, 1.0).items.len(), 360_000);
        assert_eq!(h.view(0.0, 0.5, 1.0).items.len(), 36_000);
        assert_eq!(h.view(0.0, 6.0, 1.0).items.len(), 3_600);
    }

    #[test]
    fn spikes_survive_aggregation() {
        let h = filled(60, 100);
        for spp in [0.01, 0.5, 6.0] {
            let hi = h
                .view(0.0, spp, 1.0)
                .items
                .map(|b| b.hi[0])
                .fold(0.0, f32::max);
            assert_eq!(hi, 9.0, "at {spp} s/px");
        }
    }

    #[test]
    fn views_start_at_the_window() {
        let h = filled(100, 100);
        let first = h.view(90.0, 1.0, 1.0).items.next().unwrap();
        assert!(first.t >= 89.0 && first.t <= 90.0, "{}", first.t);
    }

    #[test]
    fn old_samples_are_dropped() {
        let mut h = History::new(10.0);
        for k in 0..2000 {
            h.push(f64::from(k) / 100.0, [0.0; CHANNELS]);
        }
        assert!(h.view(0.0, 0.0, 1.0).items.next().unwrap().t >= 9.99);
        assert_eq!(h.len(), 1001);
    }

    #[test]
    fn nearest_sample() {
        let h = filled(10, 10);
        assert_eq!(h.nearest(3.04).unwrap().t, 3.0);
        assert_eq!(h.nearest(3.06).unwrap().t, 3.1);
    }
}
