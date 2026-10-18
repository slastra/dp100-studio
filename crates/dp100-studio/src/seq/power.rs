//! Constant power, which the DP100 doesn't have in hardware: its OPP
//! setting only trips the output off.
//!
//! A [`Hold`] keeps the voltage setpoint at a limit, so the supply runs in
//! constant current, and moves the current limit until V × I is on target.
//! Moving the voltage instead would be unstable on batteries and LEDs, where
//! a few millivolts change the current a lot.
//!
//! Each update is a Newton step in log terms. A load's voltage goes roughly
//! as a power of its current, V ∝ Iᵃ (a = 1 for a resistor, near 0 for a
//! battery, above 1 for a lamp filament), so its power goes as I^(1+a) and
//! `I' = I · (P / P_now)^(1 / (1 + a))` lands on target. The hold estimates
//! `a` from its last two updates, and never uses less than 1: that is the
//! geometric mean `I' = sqrt(I · P / V)`, which converges from below on
//! anything gentler than a resistor without overshooting. Until it has an
//! estimate it assumes 2, so the first step is cautious. (The plain
//! `I' = P / V` would oscillate on a resistor forever.)
//!
//! That makes it an integral controller in log terms, and it borrows the
//! usual remedies for one:
//!
//! - **Anti-windup.** In constant voltage the current limit isn't what
//!   limits, so stepping from it would wind up. There the power goes with
//!   the current at a fixed voltage, and the limit goes straight to
//!   `P / Vlimit`.
//! - **A fast limit.** Supplies with a constant power mode have a P/V limit
//!   that acts at once. Updates here are a quarter second apart, so a load
//!   that suddenly takes far more (a motor breaking free) is cut to P/V on
//!   the next couple of readings instead, and the worker polls at 100 Hz
//!   while a hold runs.
//! - **Waiting out a lag.** A load that lags (a motor changing speed) is
//!   measured once its readings stop moving, and a step that overshoots
//!   halves the next one.
//! - **Low gain on target.** Once held, a small error is corrected on a
//!   second's average and only in part, so a load's own wander (a gearbox)
//!   is smoothed rather than chased, which would add to it.
//!
//! Starting at `P / Vlimit` can't overshoot, since the load's voltage never
//! exceeds the limit.

use std::time::{Duration, Instant};

use super::exec::{Measure, Target};
use super::schema::RAMP_INTERVAL;

/// What a hold is asked for: watts, and the volts and amps it stays inside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerTarget {
    pub power: f64,
    pub max_voltage: f64,
    pub max_current: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldState {
    /// The output is off.
    Idle,
    /// On its way to the target.
    Settling,
    /// Reached the target, and regulating: on target to within the
    /// supply's 1 mA steps, or bringing a wandering load back to it.
    Holding,
    /// The load takes less than the target at the voltage limit.
    LimitedByVoltage,
    /// The current limit is reached before the target.
    LimitedByCurrent,
}

/// A write is still settling for this long after it is sent, so readings
/// from that time say nothing about the new setpoint.
const SETTLE: Duration = Duration::from_millis(100);
/// Below this the output is shorted, and V says nothing about the load.
const SHORT_V: f64 = 0.01;
/// How often a hold that is on target looks again.
const HOLDING_INTERVAL: Duration = Duration::from_secs(1);
/// The load exponent assumed before there is an estimate.
const FIRST_EXPONENT: f64 = 2.0;
/// The smallest share of a step a hold takes on a load that keeps
/// overshooting.
const MIN_GAIN: f64 = 0.25;
/// Errors a held load corrects on a second's average, taking this share of
/// a step, rather than on a quarter second's.
const SLOW_BAND: f64 = 0.10;
const HELD_GAIN: f64 = 0.3;
/// How much the power may still be moving, as a share of the target, for
/// an update on the way to it to go ahead, and how long after a write it
/// waits for that at most.
const TREND: f64 = 0.03;
const MAX_WAIT: Duration = Duration::from_millis(1500);
/// How far over the target readings go, and for how many readings in a
/// row, before a hold cuts the current at once instead of at its next
/// update. Twice the holding band, if that is wider. Well clear of a
/// load's own wander, which the updates deal with.
const SURGE: f64 = 0.25;
const SURGE_READINGS: u32 = 2;

#[derive(Debug, Clone, Copy)]
struct Reading {
    v: f64,
    i: f64,
    p: f64,
    cc: bool,
}

fn mean_power(rs: &[Reading]) -> f64 {
    rs.iter().map(|r| r.p).sum::<f64>() / rs.len() as f64
}

#[derive(Debug, Clone)]
pub struct Hold {
    want: PowerTarget,
    state: HoldState,
    /// Whether the setpoints for `want` have been put in place.
    placed: bool,
    /// The current limit last sent, in amps.
    iset: f64,
    next: Option<Instant>,
    settle_until: Option<Instant>,
    /// Readings since the last update.
    readings: Vec<Reading>,
    /// The average power at the last decision.
    power: f64,
    /// ln I and ln V at the last decision in constant current, for
    /// estimating the load exponent.
    last_cc: Option<(f64, f64)>,
    exponent: f64,
    /// Readings in a row over the surge line.
    surging: u32,
    /// Whether the power is meant to be falling: the last write lowered
    /// the limit, or the target was lowered, and no update has been made
    /// since.
    lowered: bool,
    /// The share of each step taken, halved when the error changes sign
    /// between updates (the load lags, and the last step overshot) and
    /// grown back while it doesn't.
    gain: f64,
    last_err: f64,
}

impl Hold {
    pub fn new(want: PowerTarget) -> Self {
        Self {
            want,
            state: HoldState::Settling,
            placed: false,
            iset: 0.0,
            next: None,
            settle_until: None,
            readings: Vec::new(),
            power: 0.0,
            last_cc: None,
            exponent: FIRST_EXPONENT,
            surging: 0,
            lowered: false,
            gain: 1.0,
            last_err: 0.0,
        }
    }

    pub fn want(&self) -> PowerTarget {
        self.want
    }

    pub fn state(&self) -> HoldState {
        self.state
    }

    /// The average power over the last update, in watts.
    pub fn power(&self) -> f64 {
        self.power
    }

    /// Changes what is held. A new power keeps the present current limit
    /// and corrects from there at the next update; a new voltage limit
    /// starts over from the safe starting current.
    pub fn retarget(&mut self, want: PowerTarget) {
        if want.max_voltage != self.want.max_voltage {
            self.placed = false;
            self.last_cc = None;
        } else if self.iset > want.max_current {
            self.iset = want.max_current;
            self.placed = false;
        }
        self.lowered = want.power < self.want.power;
        self.want = want;
        self.next = None;
        self.state = HoldState::Settling;
        self.gain = 1.0;
        self.last_err = 0.0;
    }

    /// Puts the setpoints where the hold starts from, if a new hold or a
    /// new voltage limit needs it. Takes no measurement, so it can run
    /// before the output comes on.
    pub fn place(&mut self, now: Instant, t: &mut impl Target) -> Result<HoldState, String> {
        if !self.placed {
            let w = self.want;
            let start = if self.iset > 0.0 {
                self.iset.min(w.max_current)
            } else {
                (w.power / w.max_voltage).min(w.max_current)
            };
            self.write(now, w.max_voltage, start, t)?;
            self.placed = true;
            self.state = HoldState::Settling;
        }
        Ok(self.state)
    }

    pub fn tick(
        &mut self,
        now: Instant,
        m: &Measure,
        t: &mut impl Target,
    ) -> Result<HoldState, String> {
        let w = self.want;
        if !self.placed {
            return self.place(now, t);
        }
        if !m.on {
            self.state = HoldState::Idle;
            self.readings.clear();
            return Ok(self.state);
        }
        // A supply with a constant power mode has a P/V limit that acts at
        // once. Updates here are a quarter second apart, so a load that
        // suddenly takes far more (a motor breaking free and spinning up)
        // is cut back to P/V as soon as it shows, the way that limit would. It
        // watches readings while a write settles too: it only ever lowers
        // the limit, and a cut it has made can't repeat, since P/V depends
        // on the voltage rather than the current still settling.
        //
        // After a write that lowered the limit, or a lower target, it waits
        // for the next update: that is the power falling as meant, and a
        // motor still coasting down holds its voltage up for a while, so a
        // cut to P/V then would undershoot.
        let surge_at = w.power + (SURGE * w.power).max(2.0 * m.voltage * 0.001);
        if m.power > surge_at && m.voltage > SHORT_V && !self.lowered {
            self.surging += 1;
            let cut = (w.power / m.voltage).min(w.max_current);
            if self.surging >= SURGE_READINGS && milli(cut) < milli(self.iset) {
                self.surging = 0;
                // The reading is mid-transient, so it says nothing about
                // the load's exponent.
                self.last_cc = None;
                self.next = None;
                self.write(now, w.max_voltage, cut, t)?;
                // A cut to P/V can follow a voltage that keeps rising.
                self.lowered = false;
                self.state = HoldState::Settling;
                return Ok(self.state);
            }
        } else {
            self.surging = 0;
        }
        if self.settle_until.is_some_and(|s| now < s) {
            return Ok(self.state);
        }
        self.readings.push(Reading {
            v: m.voltage,
            i: m.current,
            p: m.power,
            cc: m.cc,
        });
        if self.next.is_some_and(|n| now < n) {
            return Ok(self.state);
        }
        // On the way to a target, a load that lags (a motor changing
        // speed) is measured once it has settled: deciding on readings
        // still on the move would step too far. If the last third of the
        // readings differs from the first, wait on the last third, up to
        // MAX_WAIT after the write.
        let rs = &self.readings;
        let third = rs.len() / 3;
        let written = self.settle_until.map(|t| t - SETTLE);
        if self.state == HoldState::Settling
            && third >= 3
            && written.is_some_and(|t| now.saturating_duration_since(t) < MAX_WAIT)
            && (mean_power(&rs[rs.len() - third..]) - mean_power(&rs[..third])).abs()
                > TREND * w.power
        {
            self.readings.drain(..rs.len() - third);
            self.next = Some(now + RAMP_INTERVAL / 2);
            return Ok(self.state);
        }
        let rs = std::mem::take(&mut self.readings);
        self.lowered = false;
        let n = rs.len() as f64;
        let v = rs.iter().map(|r| r.v).sum::<f64>() / n;
        let i = rs.iter().map(|r| r.i).sum::<f64>() / n;
        let p = mean_power(&rs);
        let cc = rs.iter().filter(|r| r.cc).count() * 2 >= rs.len();
        self.power = p;

        let target = w.power;
        let hold_tol = (0.02 * target).max(v * 0.001);
        let deadband = (0.01 * target).max(v * 0.0005);
        let err = p - target;
        let was_holding = self.state == HoldState::Holding;

        // Learn how the load's voltage follows its current.
        if cc && v > SHORT_V && i > 0.0 {
            let here = (i.ln(), v.ln());
            if let Some((li, lv)) = self.last_cc
                && (here.0 - li).abs() > 0.05
            {
                self.exponent = ((here.1 - lv) / (here.0 - li)).clamp(1.0, 3.0);
            }
            self.last_cc = Some(here);
        }

        // The power went past the target since the last update.
        let crossed = err * self.last_err < 0.0;
        self.last_err = err;
        let ceiling = w.max_current.min(4.0 * self.iset);
        let next_i = if !cc {
            // At the voltage limit the current limit isn't what limits, so
            // stepping from it would wind up: a load that draws less than
            // the limit and over the target would take several updates
            // before a lower limit bit. The voltage is fixed here, so P/V
            // is exact. Below the target it also keeps the limit where a
            // sudden drop in the load's resistance still can't pass it.
            (target / w.max_voltage).min(w.max_current)
        } else if v < SHORT_V || p <= 0.0 {
            ceiling
        } else {
            self.gain = if crossed {
                (self.gain * 0.5).max(MIN_GAIN)
            } else {
                (self.gain * 1.5).min(1.0)
            };
            // Held, a small error is the load's own wander as much as
            // anything: a full step on it would chase that wander and add
            // to it, so only a share of the average error is taken.
            let gain = if was_holding && err.abs() <= SLOW_BAND * target {
                HELD_GAIN
            } else {
                self.gain
            };
            let step = gain / (1.0 + self.exponent);
            (self.iset * (target / p).powf(step)).min(ceiling)
        };

        // Held, a limit takes twice the band to count, so a load that sits
        // right at the voltage limit doesn't flicker between the two.
        let short_by = if was_holding { -2.0 * hold_tol } else { 0.0 };
        self.state = if err.abs() <= hold_tol || (crossed && err.abs() <= SLOW_BAND * target) {
            // Reached, as a supply's CC lamp lights once the current gets
            // to its limit: on target, or gone past it.
            HoldState::Holding
        } else if !cc && err < short_by {
            HoldState::LimitedByVoltage
        } else if err < short_by && self.iset >= w.max_current - 0.0005 {
            HoldState::LimitedByCurrent
        } else if was_holding {
            // Holding means in regulation, as a supply's CC lamp does, not
            // on target to the milliwatt: a load that wanders (a geared
            // motor) stays held while the corrections below bring it back.
            HoldState::Holding
        } else {
            HoldState::Settling
        };
        // Small errors are judged on a second's average, which smooths a
        // load's own wander instead of chasing it.
        let slow = self.state == HoldState::Holding && err.abs() <= SLOW_BAND * target;
        self.next = Some(
            now + if slow {
                HOLDING_INTERVAL
            } else {
                RAMP_INTERVAL
            },
        );

        // On target, only a drift past the holding band is worth a write.
        let band = if was_holding { hold_tol } else { deadband };
        let worth = err.abs() > band || self.state == HoldState::LimitedByVoltage;
        let mut next_i = next_i;
        if worth && milli(next_i).max(1) == milli(self.iset).max(1) && cc {
            // Too small a move for the supply's 1 mA steps: take one step,
            // or a cautious estimate would never get anywhere. The holding
            // band is at least a step wide, so this can't dither.
            next_i = (self.iset - 0.001_f64.copysign(err)).clamp(0.001, w.max_current);
        }
        if worth && milli(next_i).max(1) != milli(self.iset).max(1) {
            self.write(now, w.max_voltage, next_i, t)?;
        }
        Ok(self.state)
    }

    fn write(
        &mut self,
        now: Instant,
        volts: f64,
        amps: f64,
        t: &mut impl Target,
    ) -> Result<(), String> {
        let (v, i) = (milli(volts), milli(amps).max(1));
        t.set(v, i)?;
        self.lowered = self.placed && f64::from(i) / 1000.0 < self.iset;
        self.iset = f64::from(i) / 1000.0;
        self.settle_until = Some(now + SETTLE);
        let earliest = now + SETTLE + RAMP_INTERVAL;
        self.next = Some(self.next.map_or(earliest, |n| n.max(earliest)));
        self.readings.clear();
        Ok(())
    }
}

fn milli(x: f64) -> u16 {
    (x * 1000.0).round().clamp(0.0, f64::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A load as a function of the setpoints (V, A): what it measures and
    /// whether the supply is in constant current.
    type Plant = dyn Fn(f64, f64) -> (f64, f64, bool);

    struct Bench<'a> {
        plant: &'a Plant,
        v: u16,
        i: u16,
        writes: u32,
    }

    impl Target for Bench<'_> {
        fn setpoints(&mut self) -> Result<(u16, u16), String> {
            Ok((self.v, self.i))
        }
        fn set(&mut self, v: u16, i: u16) -> Result<(), String> {
            (self.v, self.i) = (v, i);
            self.writes += 1;
            Ok(())
        }
        fn output(&mut self, _: bool) -> Result<(), String> {
            Ok(())
        }
    }

    struct Trace {
        /// (seconds, watts, state) per poll.
        points: Vec<(f64, f64, HoldState)>,
        writes: u32,
        iset: f64,
    }

    impl Trace {
        fn last(&self) -> (f64, HoldState) {
            let p = self.points.last().unwrap();
            (p.1, p.2)
        }
        /// Seconds until the state first reads Holding.
        fn settled_at(&self) -> Option<f64> {
            self.points
                .iter()
                .find(|p| p.2 == HoldState::Holding)
                .map(|p| p.0)
        }
        fn peak(&self) -> f64 {
            self.points.iter().map(|p| p.1).fold(0.0, f64::max)
        }
    }

    /// How often a hold polls, as the worker does.
    const POLL: f64 = 0.01;

    fn run(plant: &Plant, want: PowerTarget, secs: f64) -> Trace {
        let mut hold = Hold::new(want);
        run_with(&mut hold, plant, secs, &mut |_, _| {})
    }

    fn run_with(
        hold: &mut Hold,
        plant: &Plant,
        secs: f64,
        each: &mut dyn FnMut(f64, &mut Hold),
    ) -> Trace {
        let t0 = Instant::now();
        let mut bench = Bench {
            plant,
            v: 0,
            i: 0,
            writes: 0,
        };
        let mut points = Vec::new();
        let mut k = 0u32;
        loop {
            let t = f64::from(k) * POLL;
            if t > secs {
                break;
            }
            each(t, hold);
            let (v, i, cc) =
                (bench.plant)(f64::from(bench.v) / 1000.0, f64::from(bench.i) / 1000.0);
            let m = Measure {
                voltage: v,
                current: i,
                power: v * i,
                on: true,
                cc,
                ..Measure::default()
            };
            let now = t0 + Duration::from_secs_f64(t);
            let state = hold.tick(now, &m, &mut bench).unwrap();
            points.push((t, v * i, state));
            k += 1;
        }
        Trace {
            points,
            writes: bench.writes,
            iset: f64::from(bench.i) / 1000.0,
        }
    }

    fn resistor(r: f64) -> impl Fn(f64, f64) -> (f64, f64, bool) {
        move |vset, iset| {
            let i = vset / r;
            if i <= iset {
                (vset, i, false)
            } else {
                (iset * r, iset, true)
            }
        }
    }

    /// A cell at `e` volts behind `r` ohms: it can only take current above
    /// its own voltage.
    fn battery(e: f64, r: f64) -> impl Fn(f64, f64) -> (f64, f64, bool) {
        move |vset, iset| {
            let i = ((vset - e) / r).max(0.0);
            if i <= iset {
                (vset, i, false)
            } else {
                (e + iset * r, iset, true)
            }
        }
    }

    /// An LED: about 3 V at 10 mA, rising slowly with current.
    fn led(vset: f64, iset: f64) -> (f64, f64, bool) {
        let v_at = |i: f64| 2.5 + 0.06 * (1.0 + i / 1e-6).ln();
        let i_at = |v: f64| 1e-6 * (((v - 2.5) / 0.06).exp() - 1.0).max(0.0);
        let i = i_at(vset);
        if i <= iset {
            (vset, i, false)
        } else {
            (v_at(iset), iset, true)
        }
    }

    /// A lamp filament: resistance rises with current.
    fn filament(vset: f64, iset: f64) -> (f64, f64, bool) {
        let (r0, k) = (5.0, 4.0);
        let v_at = |i: f64| i * r0 * (1.0 + k * i);
        // r0·k·i² + r0·i − v = 0
        let i_at = |v: f64| (-r0 + (r0 * r0 + 4.0 * r0 * k * v).sqrt()) / (2.0 * r0 * k);
        let i = i_at(vset);
        if i <= iset {
            (vset, i, false)
        } else {
            (v_at(iset), iset, true)
        }
    }

    /// A geared hobby motor, fitted to one on the bench: 5.5 Ω of winding
    /// and a back-EMF that follows the current with a 0.3 s lag. It sticks
    /// until 240 mA, then keeps turning down to 50 mA, its gearbox drags
    /// ±15 % harder as it turns, and its voltage ripples by ±3 % as the
    /// brushes commutate.
    fn motor() -> impl Fn(f64, f64) -> (f64, f64, bool) {
        let (r, gain, i_run, i_break, tau, dt) = (5.5, 54.0, 0.050, 0.240, 0.3, POLL);
        let emf = std::cell::Cell::new(0.0_f64);
        let k = std::cell::Cell::new(0u32);
        // At a fixed voltage the back-EMF feeds back through the current
        // in tens of milliseconds, so each poll is integrated in 1 ms steps.
        let at = move |vset: f64, iset: f64, e: f64| {
            if iset * r + e <= vset {
                (iset * r + e, iset, true)
            } else {
                (vset, ((vset - e) / r).max(0.0), false)
            }
        };
        move |vset, iset| {
            for _ in 0..10 {
                let e = emf.get();
                let i = at(vset, iset, e).1;
                let turning = e > 0.0 || i > i_break;
                let drag = i_run * (1.0 + 0.15 * (f64::from(k.get()) * dt * 3.8).sin());
                let settles_at = if turning { gain * (i - drag) } else { 0.0 };
                emf.set((e + (settles_at - e) * dt / 10.0 / tau).max(0.0));
            }
            let (v, i, cc) = at(vset, iset, emf.get());
            k.set(k.get() + 1);
            let ripple = 1.0 + 0.03 * (f64::from(k.get()) * 0.9).sin();
            (v * ripple, i, cc)
        }
    }

    fn want(power: f64, max_voltage: f64, max_current: f64) -> PowerTarget {
        PowerTarget {
            power,
            max_voltage,
            max_current,
        }
    }

    #[test]
    fn a_resistor_settles_in_one_update() {
        let tr = run(&resistor(10.0), want(1.0, 12.0, 1.0), 3.0);
        assert_eq!(tr.last().1, HoldState::Holding);
        assert!((tr.last().0 - 1.0).abs() < 0.02, "{}", tr.last().0);
        // Two updates: a cautious first step, then an exact one.
        assert!(tr.settled_at().unwrap() < 1.2, "{:?}", tr.settled_at());
        assert!((tr.iset - 0.316).abs() < 0.002, "{}", tr.iset);
        assert!(tr.peak() <= 1.02);
    }

    #[test]
    fn a_battery_converges_from_below() {
        let tr = run(&battery(3.7, 0.1), want(2.0, 4.2, 1.0), 6.0);
        assert_eq!(tr.last().1, HoldState::Holding);
        assert!(tr.peak() <= 2.0 * 1.02, "peak {}", tr.peak());
    }

    #[test]
    fn an_led_converges_from_below() {
        let tr = run(&led, want(0.025, 3.4, 0.010), 6.0);
        let (p, state) = tr.last();
        assert!((p - 0.025).abs() < 0.0035, "{p}");
        assert!(matches!(state, HoldState::Holding), "{state:?}");
        assert!(tr.peak() <= 0.025 + 0.0035, "peak {}", tr.peak());
    }

    #[test]
    fn a_filament_converges_with_a_small_overshoot() {
        let tr = run(&filament, want(3.0, 12.0, 2.0), 6.0);
        assert_eq!(tr.last().1, HoldState::Holding);
        assert!(tr.peak() <= 3.0 * 1.03, "peak {}", tr.peak());
    }

    #[test]
    fn a_light_load_is_limited_by_voltage() {
        // 100 Ω at 5 V takes only 0.25 W.
        let tr = run(&resistor(100.0), want(1.0, 5.0, 1.0), 3.0);
        assert_eq!(tr.last().1, HoldState::LimitedByVoltage);
        // Held where a drop in resistance still can't pass 1 W at 5 V.
        assert!((tr.iset - 0.2).abs() < 0.001, "{}", tr.iset);
    }

    #[test]
    fn a_heavy_load_is_limited_by_current_and_a_short_capped() {
        let tr = run(&resistor(1.0), want(5.0, 12.0, 0.5), 3.0);
        assert_eq!(tr.last().1, HoldState::LimitedByCurrent);
        assert!((tr.iset - 0.5).abs() < 1e-9);
        let short = run(&resistor(0.0001), want(5.0, 12.0, 0.5), 3.0);
        assert!((short.iset - 0.5).abs() < 1e-9, "{}", short.iset);
    }

    #[test]
    fn lowering_the_target_follows_down() {
        let mut hold = Hold::new(want(2.0, 12.0, 1.0));
        let r = resistor(10.0);
        let tr = run_with(&mut hold, &r, 4.0, &mut |t, h| {
            if (t - 2.0).abs() < 1e-9 {
                h.retarget(want(0.5, 12.0, 1.0));
            }
        });
        assert_eq!(tr.last().1, HoldState::Holding);
        assert!((tr.last().0 - 0.5).abs() < 0.01, "{}", tr.last().0);
    }

    #[test]
    fn a_motor_is_held_without_overshoot_or_flicker() {
        // What the bench motor was put through: 0.3 W from a standstill,
        // where it stays stuck, then 0.5 W, which breaks it free and spins
        // it up to the voltage limit, then 0.2 W.
        let targets = [0.3, 0.5, 0.2];
        let mut hold = Hold::new(want(targets[0], 4.5, 0.3));
        let m = motor();
        let tr = run_with(&mut hold, &m, 24.0, &mut |t, h| {
            if t > 0.0 && t < 20.0 && (t % 8.0).abs() < 1e-9 {
                h.retarget(want(targets[(t / 8.0) as usize], 4.5, 0.3));
            }
        });
        for (n, &p) in targets.iter().enumerate() {
            let phase: Vec<_> = tr
                .points
                .iter()
                .filter(|q| q.0 >= 8.0 * n as f64 && q.0 < 8.0 * (n + 1) as f64)
                .collect();
            // Over a quarter second, as the hold sees it, in the two
            // seconds after the change, once a lower target is first met.
            // After that the motor's own wander is ±15 %, which no
            // correction four times a second can follow.
            let from = phase.iter().position(|q| q.1 <= p).unwrap();
            let peak = phase[from..from + 200]
                .windows(25)
                .map(|w| w.iter().map(|q| q.1).sum::<f64>() / 25.0)
                .fold(0.0, f64::max);
            assert!(peak <= p * 1.1, "{p} W peaked at {peak:.3} W");
            let held = phase.iter().position(|q| q.2 == HoldState::Holding);
            let held = held.unwrap_or_else(|| panic!("{p} W never held"));
            assert!(held < 300, "{p} W took {} ms to hold", held * 10);
            let after = &phase[held..];
            let mean = after.iter().map(|q| q.1).sum::<f64>() / after.len() as f64;
            assert!(
                (mean / p - 1.0).abs() < 0.05,
                "{p} W held {mean:.3} W on average"
            );
            let lost = after.iter().filter(|q| q.2 != HoldState::Holding).count();
            assert!(lost * 10 <= after.len(), "{p} W held only part of the time");
        }
        assert!(tr.writes <= 30, "{} writes", tr.writes);
    }

    #[test]
    fn a_noisy_load_does_not_flood_the_supply() {
        // ±3 % of jitter on a resistor, from a fixed sequence.
        let seed = std::cell::Cell::new(0x2545_f491_u32);
        let noisy = move |vset: f64, iset: f64| {
            let mut x = seed.get();
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            seed.set(x);
            let j = 1.0 + (f64::from(x % 601) - 300.0) / 10_000.0;
            let (v, i, cc) = resistor(10.0)(vset, iset);
            (v * j, i, cc)
        };
        let tr = run(&noisy, want(1.0, 12.0, 1.0), 60.0);
        assert!(tr.writes < 30, "{} writes in a minute", tr.writes);
        assert!((tr.last().0 - 1.0).abs() < 0.05);
    }

    #[test]
    fn it_idles_while_the_output_is_off() {
        let mut hold = Hold::new(want(1.0, 12.0, 1.0));
        let mut bench = Bench {
            plant: &resistor(10.0),
            v: 0,
            i: 0,
            writes: 0,
        };
        let now = Instant::now();
        let off = Measure::default();
        hold.tick(now, &off, &mut bench).unwrap();
        assert_eq!(bench.writes, 1, "puts the setpoints in place");
        assert_eq!((bench.v, bench.i), (12_000, 83));
        let s = hold
            .tick(now + Duration::from_secs(1), &off, &mut bench)
            .unwrap();
        assert_eq!(s, HoldState::Idle);
        assert_eq!(bench.writes, 1);
    }
}
