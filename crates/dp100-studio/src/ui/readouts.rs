//! The Monitor page.
//!
//! Laid out like the front of a bench instrument: a panel of annunciator
//! lamps and the power switch, two big channel readouts (voltage and
//! current) each with a bar showing how close it sits to its setpoint, and
//! a quiet footer of secondary figures. The setpoint editor and the graph
//! follow, then the details.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode, Setpoints};

use super::controls::Controls;
use super::ctx::{amps, volts};
use super::graph::{Graph, Lane};
use super::log::{charge, energy};
use super::palette::Ink;
use super::panel::{Panel, Tone};
use super::segments::SevenSeg;
use crate::seq::power::HoldState;
use crate::worker::PowerStatus;

/// Text readouts refresh at most this often. The graph takes every sample;
/// labels repainting at 100 Hz while recording would only cost power.
const LABEL_INTERVAL: Duration = Duration::from_millis(50);

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(classes.to_vec())
        .xalign(0.0)
        .build()
}

/// One big channel readout: caption, value and unit, a headroom bar, and a
/// line saying what it is being held to.
struct Channel {
    display: SevenSeg,
    unit: &'static str,
    bar: gtk::ProgressBar,
    note: gtk::Label,
}

impl Channel {
    /// A reading: its name with its setpoint opposite, the digits, and a
    /// bar of how much of the setpoint is in use.
    fn new(caption: &str, unit: &'static str, ink: Ink) -> (Self, gtk::Box) {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .build();
        let head = gtk::Box::builder().spacing(6).build();
        head.append(
            &gtk::Label::builder()
                .label(caption)
                .css_classes(["caption-heading"])
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        let note = label("", &["caption", "dim-label", "numeric"]);
        note.set_xalign(1.0);
        head.append(&note);
        b.append(&head);

        let line = gtk::Box::builder().spacing(10).build();
        let display = SevenSeg::new(2, 3, Some(ink));
        line.append(&display);
        line.append(
            &gtk::Label::builder()
                .label(unit)
                .css_classes(["title-2", "dim-label"])
                .valign(gtk::Align::End)
                .build(),
        );
        b.append(&line);
        let bar = gtk::ProgressBar::new();
        b.append(&bar);
        (
            Self {
                display,
                unit,
                bar,
                note,
            },
            b,
        )
    }

    fn show(&self, milli: u16) {
        let v = f64::from(milli) / 1000.0;
        let name = if self.unit == "V" { "volts" } else { "amps" };
        self.display.set_value(Some(v), &format!("{v:.3} {name}"));
    }
}

/// A figure under the readings, centred in its third of the row.
fn stat(caption: &str) -> (gtk::Label, gtk::Box) {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::Center)
        .build();
    let c = label(caption, &["caption-heading", "dim-label"]);
    c.set_xalign(0.5);
    b.append(&c);
    let v = label("--", &["heading", "numeric"]);
    v.set_xalign(0.5);
    b.append(&v);
    (v, b)
}

/// The panel's status lamps, in the order a front panel would print them.
const LAMPS: [(&str, &str, Tone); 8] = [
    ("CV", "Regulating voltage", Tone::Ok),
    ("CC", "Limiting current", Tone::Warn),
    ("CP", "Holding a constant power", Tone::Info),
    ("OUT", "Output on", Tone::Alert),
    ("OVP", "Over-voltage trip", Tone::Alert),
    ("OCP", "Over-current trip", Tone::Alert),
    ("OPP", "Over-power trip", Tone::Alert),
    ("OTP", "Over-temperature trip", Tone::Alert),
];

/// The time windows offered, in seconds.
/// The graph's time windows: a short label, as charting tools write them,
/// and what it says in full.
const WINDOWS: [(&str, &str, f64); 6] = [
    ("10s", "Last 10 seconds", 10.0),
    ("30s", "Last 30 seconds", 30.0),
    ("1m", "Last minute", 60.0),
    ("5m", "Last 5 minutes", 300.0),
    ("15m", "Last 15 minutes", 900.0),
    ("1h", "Last hour", 3600.0),
];

/// A row of toggle buttons drawn as one segmented control. Clicking one
/// doesn't take keyboard focus, so no focus ring is left behind; Tab still
/// reaches them.
fn segmented() -> gtk::Box {
    gtk::Box::builder().css_classes(["linked"]).build()
}

fn segment(label: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .label(label)
        .focus_on_click(false)
        .build()
}

/// The graph with its window, lane and pause controls above it. Returns the
/// section, the control bar and the lane toggles, which a narrow window
/// stacks.
fn graph_section(graph: &Graph) -> (gtk::Box, gtk::Box, gtk::Box) {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    // The two halves wrap rather than hold one long row: a page whose
    // content can't get narrow is clamped wider than the others.
    let bar = gtk::Box::builder().spacing(12).build();

    let windows = segmented();
    let mut first: Option<gtk::ToggleButton> = None;
    for (text, full, secs) in WINDOWS {
        let b = segment(text);
        b.set_tooltip_text(Some(full));
        b.update_property(&[gtk::accessible::Property::Label(full)]);
        b.set_active(secs == 30.0);
        if let Some(f) = &first {
            b.set_group(Some(f));
        } else {
            first = Some(b.clone());
        }
        let g = graph.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                g.set_window(secs);
            }
        });
        windows.append(&b);
    }
    windows.set_halign(gtk::Align::Start);
    bar.append(&windows);

    // Lane toggles and pause stay together, so a narrow window can move
    // them under the time windows as one row.
    let right = gtk::Box::builder()
        .spacing(12)
        .hexpand(true)
        .halign(gtk::Align::End)
        .build();
    let lanes = segmented();
    lanes.add_css_class("lanes");
    for lane in Lane::ALL {
        let b = segment(lane.short());
        b.add_css_class(lane.css_class());
        b.set_tooltip_text(Some(&format!("Show {}", lane.name())));
        b.set_active(graph.lane_shown(lane));
        let g = graph.clone();
        b.connect_toggled(move |b| g.set_lane(lane, b.is_active()));
        lanes.append(&b);
    }
    right.append(&lanes);

    let pause = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause the Graph")
        .css_classes(["flat", "circular"])
        .focus_on_click(false)
        .build();
    pause.update_property(&[gtk::accessible::Property::Label("Pause the graph")]);
    let g = graph.clone();
    pause.connect_toggled(move |b| {
        g.set_paused(b.is_active());
        b.set_icon_name(if b.is_active() {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        });
    });
    right.append(&pause);
    bar.append(&right);

    section.append(&bar);
    // On the window's own background, not in a card: the graph is the page.
    section.append(graph);
    (section, bar, right)
}

/// An `AdwActionRow` whose suffix label is updated in place.
fn row(group: &adw::PreferencesGroup, title: &str) -> gtk::Label {
    let value = gtk::Label::builder()
        .css_classes(["numeric", "dim-label"])
        .selectable(true)
        .build();
    let r = adw::ActionRow::builder().title(title).build();
    r.add_suffix(&value);
    group.add(&r);
    value
}

/// The narrowest and widest the panel of readouts and setpoints goes beside
/// the graph.
pub const PANEL_WIDTH: i32 = 340;
pub const PANEL_MAX_WIDTH: i32 = 480;
/// The inset of the panel and the graph from the window's edges and the
/// handle between them.
const GUTTER: i32 = 12;

pub struct Readouts {
    /// The panel's contents: lamps and readings, setpoints, details.
    pub pane: gtk::Box,
    /// The graph and its toolbar, which fill the rest of the window.
    pub graph_area: gtk::Box,
    panel: Panel,
    graph_bar: gtk::Box,
    lane_toggles: gtk::Box,
    controls: Rc<Controls>,
    graph: Graph,
    volts: Channel,
    amps: Channel,
    power: gtk::Label,
    vin: gtk::Label,
    temp: gtk::Label,
    protection: gtk::Label,
    delivered: gtk::Label,
    rail: gtk::Label,
    last_labels: Cell<Option<Instant>>,
    setpoints: Cell<Option<(u16, u16)>>,
}

impl Readouts {
    pub fn new(controls: Rc<Controls>) -> Self {
        // The same inset as the graph's, so the two line up across the
        // handle.
        let pane = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(GUTTER)
            .margin_bottom(GUTTER)
            .margin_start(GUTTER)
            .margin_end(GUTTER)
            .build();

        // --- the instrument panel, laid out for a column ---
        let panel = Panel::new(&LAMPS, None);
        panel.compact(None);
        // The mode and output lamps over the four trips, as two rows.
        panel.lamps_per_line(4);

        let channels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();
        // The DP100's own colours, as the graph's lanes have them.
        let (volts, b) = Channel::new("VOLTAGE", "V", Ink::Volt);
        channels.append(&b);
        let (amps, b) = Channel::new("CURRENT", "A", Ink::Amp);
        channels.append(&b);
        panel.body.append(&channels);

        let footer = gtk::Box::builder().spacing(12).homogeneous(true).build();
        let (power, b) = stat("POWER");
        footer.append(&b);
        let (vin, b) = stat("INPUT");
        footer.append(&b);
        let (temp, b) = stat("TEMP");
        b.set_tooltip_text(Some("Temperature"));
        footer.append(&b);
        panel.body.append(&footer);
        pane.append(&panel.root);

        pane.append(&controls.setpoint);

        let output = adw::PreferencesGroup::new();
        let protection = row(&output, "Protection trips");
        let delivered = row(&output, "Delivered since connecting");
        let rail = row(&output, "Internal 5 V rail");
        pane.append(&output);

        let graph = Graph::default();
        if let Some(secs) = std::env::var("DP100_STUDIO_PREFILL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            graph.prefill(secs);
        }
        let (graph_area, graph_bar, lane_toggles) = graph_section(&graph);
        graph_area.set_hexpand(true);
        graph_area.set_vexpand(true);
        graph_area.set_margin_top(GUTTER);
        graph_area.set_margin_bottom(GUTTER);
        graph_area.set_margin_start(GUTTER);
        graph_area.set_margin_end(GUTTER);

        Self {
            pane,
            graph_area,
            panel,
            graph_bar,
            lane_toggles,
            controls,
            graph,
            volts,
            amps,
            power,
            vin,
            temp,
            protection,
            delivered,
            rail,
            last_labels: Cell::new(None),
            setpoints: Cell::new(None),
        }
    }

    /// Puts `w` in the panel under the readings, as the running sequence's
    /// card is.
    pub fn add_under_readings(&self, w: &impl IsA<gtk::Widget>) {
        self.pane.insert_child_after(w, Some(&self.panel.root));
    }

    /// Puts `w` in the panel under the setpoints.
    pub fn add_under_setpoints(&self, w: &impl IsA<gtk::Widget>) {
        self.pane
            .insert_child_after(w, Some(&self.controls.setpoint));
    }

    /// On a narrow window the lane toggles go under the time windows.
    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        bp.add_setter(
            &self.graph_bar,
            "orientation",
            Some(&gtk::Orientation::Vertical.to_value()),
        );
        bp.add_setter(
            &self.lane_toggles,
            "halign",
            Some(&gtk::Align::Start.to_value()),
        );
    }

    pub fn set_locked(&self, locked: bool) {
        self.controls.set_locked(locked);
    }

    pub fn show_power(&self, st: &PowerStatus) {
        self.controls.show_power(st);
        self.lamp(2, st.state == HoldState::Holding);
    }

    pub fn power_ended(&self) {
        self.controls.power_ended();
        self.lamp(2, false);
    }

    /// An action was refused; see [`Controls::refused`].
    pub fn refused(&self) {
        self.controls.refused();
    }

    pub fn is_holding(&self) -> bool {
        self.controls.is_holding()
    }

    fn lamp(&self, k: usize, lit: bool) {
        self.panel.lamp(k, lit);
    }

    pub fn show_sample(&self, at: Instant, m: &BasicInfo) {
        self.graph.push(at, m);
        if self
            .last_labels
            .get()
            .is_some_and(|t| at.saturating_duration_since(t) < LABEL_INTERVAL)
        {
            return;
        }
        self.last_labels.set(Some(at));
        self.controls.show_sample(m);

        self.volts.show(m.vout_mv);
        self.amps.show(m.iout_ma);
        self.power
            .set_label(&format!("{:.3}\u{202f}W", f64::from(m.power_mw()) / 1000.0));
        self.vin.set_label(&volts(m.vin_mv));
        self.temp.set_label(&format!(
            "{:.1}\u{202f}°C",
            f64::from(m.temp1_dc.max(m.temp2_dc)) / 10.0
        ));
        self.rail.set_label(&volts(m.dc5v_mv));

        // Headroom: how much of the setpoint and the current limit is in use.
        // In CC the current bar sits full and the voltage bar falls back.
        if let Some((vset, iset)) = self.setpoints.get() {
            let frac = |x: u16, of: u16| {
                if of == 0 {
                    0.0
                } else {
                    (f64::from(x) / f64::from(of)).clamp(0.0, 1.0)
                }
            };
            self.volts.bar.set_fraction(frac(m.vout_mv, vset));
            self.amps.bar.set_fraction(frac(m.iout_ma, iset));
        }

        let on = m.output_on().unwrap_or(false);
        self.lamp(0, on && m.mode == Mode::ConstantVoltage);
        self.lamp(1, on && m.mode == Mode::ConstantCurrent);
        self.lamp(3, on);
        self.lamp(4, m.fault == Fault::OverVoltage);
        self.lamp(5, m.fault == Fault::OverCurrent);
        self.lamp(6, m.fault == Fault::OverPower);
        self.lamp(7, m.fault == Fault::OverTemperature);
    }

    pub fn show_output(&self, o: &Setpoints) {
        self.controls.show_output(o);
        self.setpoints.set(Some((o.vset_mv, o.iset_ma)));
        self.volts
            .note
            .set_label(&format!("set {}", volts(o.vset_mv)));
        self.amps
            .note
            .set_label(&format!("limit {}", amps(o.iset_ma)));
        self.protection
            .set_label(&format!("OVP {} · OCP {}", volts(o.ovp_mv), amps(o.ocp_ma)));
    }

    pub fn show_energy(&self, mwh: f64, mah: f64) {
        self.delivered
            .set_label(&format!("{} · {}", energy(mwh), charge(mah)));
    }
}
