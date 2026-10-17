//! Live plot of the measurements.
//!
//! Stacked lanes (voltage, current, power, temperature) share one time axis.
//! Data comes from a [`History`] at the resolution the zoom needs, and is
//! reduced to one min/max column per pixel: the envelope is filled as a band
//! and a line runs through the latest values, so a one-sample spike still
//! shows at any zoom.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, gsk};

use super::history::{Bucket, CHANNELS, History};

/// The longest window, and so how much history is kept.
pub const HISTORY: Duration = Duration::from_secs(3600);
/// Consecutive samples further apart than this are drawn with a gap between
/// them (a disconnect, a paused worker).
const GAP: f64 = 1.0;
/// The right edge trails real time by this much, so the newest sample has
/// arrived by the time its moment scrolls into view.
const LAG: f64 = 0.12;

const LEFT: f32 = 64.0;
const RIGHT: f32 = 8.0;
const TOP: f32 = 6.0;
const BOTTOM: f32 = 20.0;
const LANE_GAP: f32 = 10.0;

/// One pixel column of reduced data.
struct Column {
    x: f32,
    lo: f32,
    hi: f32,
    last: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Voltage,
    Current,
    Power,
    Temperature,
}

impl Lane {
    pub const ALL: [Lane; 4] = [Lane::Voltage, Lane::Current, Lane::Power, Lane::Temperature];

    pub fn short(self) -> &'static str {
        match self {
            Lane::Voltage => "V",
            Lane::Current => "A",
            Lane::Power => "W",
            Lane::Temperature => "°C",
        }
    }

    /// Matches the readout panel's channel colours in style.css.
    pub fn css_class(self) -> &'static str {
        match self {
            Lane::Voltage => "volt",
            Lane::Current => "amp",
            Lane::Power => "watt",
            Lane::Temperature => "heat",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            Lane::Voltage => "VOLTAGE",
            Lane::Current => "CURRENT",
            Lane::Power => "POWER",
            Lane::Temperature => "TEMPERATURE",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lane::Voltage => "Voltage",
            Lane::Current => "Current",
            Lane::Power => "Power",
            Lane::Temperature => "Temperature",
        }
    }

    /// Channel index in [`History`] buckets.
    fn index(self) -> usize {
        self as usize
    }

    /// The GNOME palette's blue, orange, purple and red.
    fn color(self) -> gdk::RGBA {
        let (r, g, b): (u8, u8, u8) = match self {
            Lane::Voltage => (0x35, 0x84, 0xe4),
            Lane::Current => (0xff, 0x78, 0x00),
            Lane::Power => (0x91, 0x41, 0xac),
            Lane::Temperature => (0xe0, 0x1b, 0x24),
        };
        gdk::RGBA::new(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        )
    }

    /// Smallest full-scale range, so a flat zero line doesn't zoom into noise.
    fn min_span(self) -> f32 {
        match self {
            Lane::Voltage => 1.0,
            Lane::Current => 0.01,
            Lane::Power => 0.01,
            Lane::Temperature => 2.0,
        }
    }

    fn decimals(self, span: f32) -> usize {
        match self {
            Lane::Temperature => 1,
            _ if span < 0.1 => 3,
            _ if span < 1.0 => 2,
            _ => 1,
        }
    }

    /// Voltage, current and power are plotted from zero; temperature hugs
    /// its range, since it never goes near zero.
    fn zero_based(self) -> bool {
        self != Lane::Temperature
    }
}

/// Rounds up to 1, 2 or 5 times a power of ten.
fn nice_ceil(x: f32) -> f32 {
    if x <= 0.0 {
        return 1.0;
    }
    let mag = 10f32.powf(x.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if x <= m * mag * 1.000_1 {
            return m * mag;
        }
    }
    10.0 * mag
}

/// A tick spacing that reads well on a clock, giving four to eight ticks.
fn time_step(window: f64) -> f64 {
    const STEPS: [f64; 12] = [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0,
    ];
    STEPS
        .into_iter()
        .find(|&s| window / s <= 8.0)
        .unwrap_or(3600.0)
}

fn format_ago(secs: f64) -> String {
    if secs < 0.5 {
        "now".into()
    } else if secs < 120.0 {
        format!("-{secs:.0} s")
    } else {
        format!("-{:.0} min", secs / 60.0)
    }
}

mod imp {
    use super::*;

    pub struct Graph {
        pub history: RefCell<History>,
        pub origin: Cell<Option<Instant>>,
        pub window: Cell<f64>,
        pub paused: Cell<bool>,
        /// The time at the right edge while paused.
        pub frozen_end: Cell<f64>,
        /// The right-edge time of the last frame drawn.
        pub drawn_end: Cell<f64>,
        /// New data since the last frame.
        pub dirty: Cell<bool>,
        pub lanes: RefCell<Vec<Lane>>,
        pub hover_x: Cell<Option<f32>>,
        /// When the accessible description was last brought up to date.
        pub described_at: Cell<f64>,
    }

    impl Default for Graph {
        fn default() -> Self {
            Self {
                history: RefCell::new(History::new(HISTORY.as_secs_f64())),
                origin: Cell::new(None),
                window: Cell::new(30.0),
                paused: Cell::new(false),
                frozen_end: Cell::new(0.0),
                drawn_end: Cell::new(0.0),
                dirty: Cell::new(false),
                lanes: RefCell::new(vec![Lane::Voltage, Lane::Current, Lane::Power]),
                hover_x: Cell::new(None),
                described_at: Cell::new(f64::NEG_INFINITY),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Graph {
        const NAME: &'static str = "Dp100Graph";
        type Type = super::Graph;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // To assistive technology the graph is a picture, described in
            // words by its latest readings.
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for Graph {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj()
                .update_property(&[gtk::accessible::Property::Label(
                    "Graph of the output voltage, current, power and temperature",
                )]);
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            let motion = gtk::EventControllerMotion::new();
            let w = obj.downgrade();
            motion.connect_motion(move |_, x, _| {
                if let Some(w) = w.upgrade() {
                    w.imp().hover_x.set(Some(x as f32));
                    w.queue_draw();
                }
            });
            let w = obj.downgrade();
            motion.connect_leave(move |_| {
                if let Some(w) = w.upgrade() {
                    w.imp().hover_x.set(None);
                    w.queue_draw();
                }
            });
            obj.add_controller(motion);

            // Scrolling follows the display's frame clock rather than sample
            // arrivals: redrawing only when a sample lands moved the chart in
            // whole-pixel steps whose size beat against the sample rate, so it
            // visibly sped up and slowed down. A frame is only drawn once the
            // chart has moved a quarter pixel or new data came in, so long
            // windows don't redraw at the display rate for nothing.
            obj.add_tick_callback(|w, _| {
                let imp = w.imp();
                if !imp.paused.get() && imp.origin.get().is_some() {
                    let spp =
                        imp.window.get() / f64::from((w.width() as f32 - LEFT - RIGHT).max(1.0));
                    let moved = (imp.live_end() - imp.drawn_end.get()).abs() / spp;
                    if moved >= 0.25 || imp.dirty.replace(false) {
                        w.queue_draw();
                    }
                }
                glib::ControlFlow::Continue
            });
        }
    }

    impl WidgetImpl for Graph {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Vertical => {
                    let n = self.lanes.borrow().len().max(1) as i32;
                    let h = n * 110 + (TOP + BOTTOM) as i32;
                    (h, h, -1, -1)
                }
                _ => (240, 600, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let t0 = Instant::now();
            self.draw(snapshot);
            if std::env::var_os("DP100_STUDIO_TRACE_DRAW").is_some() {
                eprintln!("draw {:.2} ms", t0.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    impl Graph {
        /// Real time on the history's clock, less the display lag.
        pub fn live_end(&self) -> f64 {
            self.origin
                .get()
                .map_or(0.0, |o| o.elapsed().as_secs_f64() - LAG)
        }

        fn end(&self) -> f64 {
            if self.paused.get() {
                self.frozen_end.get()
            } else {
                self.live_end()
            }
        }

        fn draw(&self, snap: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f32;
            let h = obj.height() as f32;
            let lanes = self.lanes.borrow().clone();
            if lanes.is_empty() || w < LEFT + RIGHT + 20.0 {
                return;
            }
            let fg = obj.color();
            let grid = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08);
            let faint = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.04);
            let dim = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.55);
            // Axis text is set smaller than the body, with tabular figures so
            // scale labels don't shift as they change.
            let small = {
                let mut fd = obj.pango_context().font_description().unwrap_or_default();
                fd.set_size((f64::from(fd.size()) * 0.82) as i32);
                fd
            };
            let small_bold = {
                let mut fd = small.clone();
                fd.set_weight(gtk::pango::Weight::Heavy);
                fd.set_size((f64::from(fd.size()) * 0.9) as i32);
                fd
            };
            let tnum = gtk::pango::AttrList::new();
            tnum.insert(gtk::pango::AttrFontFeatures::new("tnum"));
            let text = |s: &str, fd: &gtk::pango::FontDescription| {
                let l = obj.create_pango_layout(Some(s));
                l.set_font_description(Some(fd));
                l.set_attributes(Some(&tnum));
                l
            };

            let plot_w = w - LEFT - RIGHT;
            let n = lanes.len() as f32;
            let lane_h = (h - TOP - BOTTOM - LANE_GAP * (n - 1.0)) / n;
            if lane_h < 20.0 {
                return;
            }

            let window = self.window.get();
            let end = self.end();
            self.drawn_end.set(end);
            // Pixel columns are fixed in absolute time: a sample always lands
            // in column floor(t / spp), whatever the window's position.
            // Measuring columns from the moving window edge made each sample
            // hop between neighbouring columns from frame to frame, so spikes
            // jittered against each other as the chart scrolled. The columns
            // themselves are then placed at sub-pixel positions, so the chart
            // moves at a constant speed.
            let spp = window / f64::from(plot_w);
            let end_col = end / spp;
            let right = LEFT + plot_w;
            let start = end - window - spp;
            let x_of = |t: f64| right - (end_col - t / spp) as f32;
            let col_x = |t: f64| right - (end_col - (t / spp).floor()) as f32;
            let history = self.history.borrow();
            let view = history.view(start, spp, GAP);
            let (visible, gap_after) = (view.items, view.gap);

            // Time grid and labels.
            let step = time_step(window);
            let mut ago = 0.0;
            // Labels go right to left from "now"; one that would touch its
            // right-hand neighbour is skipped.
            let mut free_right = f32::MAX;
            while ago <= window + 1e-6 {
                let x = x_of(end - ago);
                snap.append_color(&faint, &graphene::Rect::new(x, TOP, 1.0, h - TOP - BOTTOM));
                let layout = text(&format_ago(ago), &small);
                let (lw, _) = layout.pixel_size();
                let lx = (x - lw as f32 / 2.0).clamp(LEFT, w - RIGHT - lw as f32);
                if lx + lw as f32 + 8.0 <= free_right {
                    snap.save();
                    snap.translate(&graphene::Point::new(lx, h - BOTTOM + 3.0));
                    snap.append_layout(&layout, &dim);
                    snap.restore();
                    free_right = lx;
                }
                ago += step;
            }

            let hover_t = self
                .hover_x
                .get()
                .filter(|&x| x >= LEFT && x <= w - RIGHT)
                .map(|x| (end_col - f64::from(right - x)) * spp);
            let hover_point: Option<Bucket> = hover_t.and_then(|t| history.nearest(t));

            for (k, lane) in lanes.iter().enumerate() {
                let top = TOP + k as f32 * (lane_h + LANE_GAP);
                let ch = lane.index();
                let (mut lo, mut hi) = (f32::MAX, f32::MIN);
                for b in visible.clone() {
                    lo = lo.min(b.lo[ch]);
                    hi = hi.max(b.hi[ch]);
                }
                if lo > hi {
                    (lo, hi) = (0.0, 0.0);
                }
                let (y0, y1) = if lane.zero_based() {
                    (0.0, nice_ceil((hi * 1.1).max(lane.min_span())))
                } else {
                    let mid = (lo + hi) / 2.0;
                    let half = ((hi - lo) * 0.6).max(lane.min_span() / 2.0);
                    (mid - half, mid + half)
                };
                let y_of = |v: f32| top + lane_h - (v - y0) / (y1 - y0) * lane_h;

                // Lane frame: baseline, top line, and the scale labels.
                snap.append_color(&grid, &graphene::Rect::new(LEFT, top, plot_w, 1.0));
                snap.append_color(&grid, &graphene::Rect::new(LEFT, top + lane_h, plot_w, 1.0));
                snap.append_color(
                    &faint,
                    &graphene::Rect::new(LEFT, top + lane_h / 2.0, plot_w, 1.0),
                );
                let dec = lane.decimals(y1 - y0);
                // Zero-based lanes only need their full scale printed; the
                // baseline is zero, and a "0" there would sit against the
                // next lane's full-scale label.
                let marks: &[(f32, f32)] = if lane.zero_based() {
                    &[(y1, top)]
                } else {
                    &[(y1, top), (y0, top + lane_h - 13.0)]
                };
                for &(v, y) in marks {
                    let layout = text(&format!("{v:.dec$}\u{202f}{}", lane.short()), &small);
                    let (lw, _) = layout.pixel_size();
                    snap.save();
                    snap.translate(&graphene::Point::new(LEFT - 6.0 - lw as f32, y));
                    snap.append_layout(&layout, &dim);
                    snap.restore();
                }

                // Reduce to one column per pixel: its min, max and last value.
                // The envelope is filled as one band and a single line runs
                // through the last values. Both are simple shapes however
                // dense the data, where a stroke zig-zagging through every
                // column's extremes was costly to rasterize.
                let mut runs: Vec<Vec<Column>> = Vec::new();
                let mut last_t = f64::NEG_INFINITY;
                for b in visible.clone() {
                    let x = col_x(b.t);
                    let gap = b.t - last_t > gap_after;
                    last_t = b.t;
                    if gap || runs.is_empty() {
                        runs.push(Vec::new());
                    }
                    let run = runs.last_mut().expect("just pushed");
                    match run.last_mut() {
                        Some(c) if c.x == x => {
                            c.lo = c.lo.min(b.lo[ch]);
                            c.hi = c.hi.max(b.hi[ch]);
                            c.last = b.last[ch];
                        }
                        _ => run.push(Column {
                            x,
                            lo: b.lo[ch],
                            hi: b.hi[ch],
                            last: b.last[ch],
                        }),
                    }
                }
                let color = lane.color();
                let tint = |a: f32| gdk::RGBA::new(color.red(), color.green(), color.blue(), a);
                let band = tint(0.35);
                let line = gsk::PathBuilder::new();
                let fill = gsk::PathBuilder::new();
                // The area under the trace, shaded towards the baseline.
                let area = gsk::PathBuilder::new();
                let base = y_of(y0.max(0.0).min(y1)).min(top + lane_h);
                let mut any_band = false;
                for run in &runs {
                    let (Some(first), Some(end)) = (run.first(), run.last()) else {
                        continue;
                    };
                    area.move_to(first.x, base);
                    for c in run {
                        area.line_to(c.x, y_of(c.last));
                    }
                    area.line_to(end.x, base);
                    area.close();
                    line.move_to(first.x, y_of(first.last));
                    for c in run {
                        line.line_to(c.x, y_of(c.last));
                    }
                    // Extend a lone sample so it is visible as a short dash.
                    if run.len() == 1 {
                        line.line_to(end.x + 2.0, y_of(end.last));
                    }
                    if run.iter().any(|c| y_of(c.lo) - y_of(c.hi) > 1.0) {
                        any_band = true;
                        fill.move_to(first.x, y_of(first.hi));
                        for c in run {
                            fill.line_to(c.x, y_of(c.hi));
                        }
                        for c in run.iter().rev() {
                            fill.line_to(c.x, y_of(c.lo));
                        }
                        fill.close();
                    }
                }
                let lane_rect = graphene::Rect::new(LEFT, top - 1.0, plot_w, lane_h + 2.0);
                snap.push_clip(&lane_rect);
                if !runs.is_empty() {
                    snap.push_fill(&area.to_path(), gsk::FillRule::Winding);
                    snap.append_linear_gradient(
                        &lane_rect,
                        &graphene::Point::new(0.0, top),
                        &graphene::Point::new(0.0, top + lane_h),
                        &[
                            gsk::ColorStop::new(0.0, tint(0.30)),
                            gsk::ColorStop::new(1.0, tint(0.02)),
                        ],
                    );
                    snap.pop();
                }
                if any_band {
                    snap.append_fill(&fill.to_path(), gsk::FillRule::Winding, &band);
                }
                let stroke = gsk::Stroke::new(1.75);
                stroke.set_line_join(gsk::LineJoin::Round);
                snap.append_stroke(&line.to_path(), &stroke, &color);
                snap.pop();

                // The newest sample, marked while live, so it reads as a
                // trace being drawn rather than a picture of one.
                if !self.paused.get()
                    && let Some(c) = runs.last().and_then(|r| r.last())
                    && c.x > right - 12.0
                {
                    let p = graphene::Point::new(c.x, y_of(c.last));
                    let glow = gsk::PathBuilder::new();
                    glow.add_circle(&p, 6.0);
                    snap.append_fill(&glow.to_path(), gsk::FillRule::Winding, &tint(0.22));
                    let dot = gsk::PathBuilder::new();
                    dot.add_circle(&p, 3.0);
                    snap.append_fill(&dot.to_path(), gsk::FillRule::Winding, &color);
                }

                // Lane name, as a coloured chip in the corner.
                let chip = text(lane.caption(), &small_bold);
                let (cw, chh) = chip.pixel_size();
                let chip_rect =
                    graphene::Rect::new(LEFT + 8.0, top + 6.0, cw as f32 + 12.0, chh as f32 + 4.0);
                snap.push_rounded_clip(&gsk::RoundedRect::from_rect(chip_rect, 5.0));
                snap.append_color(&tint(0.16), &chip_rect);
                snap.pop();
                snap.save();
                snap.translate(&graphene::Point::new(LEFT + 14.0, top + 8.0));
                snap.append_layout(&chip, &color);
                snap.restore();

                if let Some(p) = hover_point {
                    let v = p.last[ch];
                    let y = y_of(v);
                    let x = col_x(p.t);
                    snap.append_color(
                        &lane.color(),
                        &graphene::Rect::new(x - 2.5, y - 2.5, 5.0, 5.0),
                    );
                    let layout = text(&format!("{v:.3}\u{202f}{}", lane.short()), &small_bold);
                    let (lw, lh) = layout.pixel_size();
                    let lx = if x + 8.0 + lw as f32 > w - RIGHT {
                        x - 8.0 - lw as f32
                    } else {
                        x + 8.0
                    };
                    let ly = (y - lh as f32 / 2.0).clamp(top, top + lane_h - lh as f32);
                    snap.save();
                    snap.translate(&graphene::Point::new(lx, ly));
                    snap.append_layout(&layout, &fg);
                    snap.restore();
                }
            }

            if let Some(p) = hover_point {
                let x = col_x(p.t);
                snap.append_color(&dim, &graphene::Rect::new(x, TOP, 1.0, h - TOP - BOTTOM));
            }
        }
    }
}

glib::wrapper! {
    pub struct Graph(ObjectSubclass<imp::Graph>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Graph {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Graph {
    pub fn push(&self, at: Instant, m: &dp100::BasicInfo) {
        let imp = self.imp();
        let origin = *imp.origin.get().get_or_insert(at);
        imp.origin.set(Some(origin));
        let t = at.saturating_duration_since(origin).as_secs_f64();
        imp.history.borrow_mut().push(
            t,
            [
                f32::from(m.vout_mv) / 1000.0,
                f32::from(m.iout_ma) / 1000.0,
                m.power_mw() as f32 / 1000.0,
                f32::from(m.temp1_dc) / 10.0,
            ],
        );
        imp.dirty.set(true);
        // Once a second is enough for a screen reader, and cheap.
        if t - imp.described_at.get() >= 1.0 {
            imp.described_at.set(t);
            self.update_property(&[gtk::accessible::Property::Description(&format!(
                "Latest: {:.3} volts, {:.3} amps, {:.3} watts, {:.1} degrees",
                f32::from(m.vout_mv) / 1000.0,
                f32::from(m.iout_ma) / 1000.0,
                m.power_mw() as f32 / 1000.0,
                f32::from(m.temp1_dc) / 10.0,
            ))]);
        }
    }

    pub fn set_window(&self, secs: f64) {
        self.imp().window.set(secs);
        self.queue_draw();
    }

    pub fn set_paused(&self, paused: bool) {
        let imp = self.imp();
        if paused {
            imp.frozen_end.set(imp.live_end());
        }
        imp.paused.set(paused);
        self.queue_draw();
    }

    pub fn set_lane(&self, lane: Lane, shown: bool) {
        let imp = self.imp();
        let mut lanes = imp.lanes.borrow_mut();
        lanes.retain(|l| *l != lane);
        if shown {
            lanes.push(lane);
            lanes.sort_by_key(|l| Lane::ALL.iter().position(|a| a == l));
        }
        drop(lanes);
        self.queue_resize();
    }

    pub fn lane_shown(&self, lane: Lane) -> bool {
        self.imp().lanes.borrow().contains(&lane)
    }

    /// Fills the history with `secs` of synthetic 100 Hz data and shows the
    /// whole hour, to measure drawing cost at full load.
    pub fn prefill(&self, secs: u32) {
        let imp = self.imp();
        let mut history = imp.history.borrow_mut();
        for k in 0..secs * 100 {
            let t = f64::from(k) / 100.0;
            let wobble = (t * 0.7).sin() as f32;
            let spike = if k % 9_000 == 0 { 0.03 } else { 0.0 };
            let v: [f32; CHANNELS] = [
                5.0 + 0.01 * wobble,
                0.05 + 0.002 * wobble + spike,
                0.25 + 0.01 * wobble,
                30.0 + wobble,
            ];
            history.push(t, v);
        }
        drop(history);
        imp.origin
            .set(Some(Instant::now() - Duration::from_secs(secs.into())));
        imp.window.set(
            std::env::var("DP100_STUDIO_PREFILL_WINDOW")
                .ok()
                .and_then(|w| w.parse().ok())
                .unwrap_or(3600.0),
        );
        self.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ceilings() {
        for (x, want) in [
            (0.0091, 0.01),
            (3.3, 5.0),
            (5.0, 5.0),
            (11.0, 20.0),
            (6.0, 10.0),
        ] {
            let got = nice_ceil(x);
            assert!(
                (got - want).abs() < want * 1e-5,
                "{x} -> {got}, want {want}"
            );
        }
    }

    #[test]
    fn clock_friendly_ticks() {
        assert_eq!(time_step(10.0), 2.0);
        assert_eq!(time_step(30.0), 5.0);
        assert_eq!(time_step(60.0), 10.0);
        assert_eq!(time_step(300.0), 60.0);
        assert_eq!(time_step(900.0), 120.0);
        assert_eq!(time_step(3600.0), 600.0);
    }

    #[test]
    fn ago_labels() {
        assert_eq!(format_ago(0.0), "now");
        assert_eq!(format_ago(30.0), "-30 s");
        assert_eq!(format_ago(600.0), "-10 min");
    }
}
