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
use super::page::Page;
use super::panel::{Panel, Tone};

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
    value: gtk::Label,
    bar: gtk::ProgressBar,
    note: gtk::Label,
}

impl Channel {
    fn new(caption: &str, unit: &str, class: &str) -> (Self, gtk::Box) {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .css_classes(["channel", class])
            .build();
        b.append(&label(caption, &["channel-caption"]));
        let line = gtk::Box::builder().spacing(6).build();
        let value = gtk::Label::builder()
            .label("--")
            .css_classes(["reading", "numeric"])
            .valign(gtk::Align::BaselineFill)
            .build();
        let unit = gtk::Label::builder()
            .label(unit)
            .css_classes(["reading-unit"])
            .valign(gtk::Align::BaselineFill)
            .build();
        line.append(&value);
        line.append(&unit);
        b.append(&line);
        let bar = gtk::ProgressBar::builder()
            .css_classes(["headroom", class])
            .build();
        b.append(&bar);
        let note = label("", &["channel-note", "numeric"]);
        b.append(&note);
        (Self { value, bar, note }, b)
    }
}

/// A small secondary figure for the panel footer.
fn stat(caption: &str) -> (gtk::Label, gtk::Box) {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    b.append(&label(caption, &["stat-caption"]));
    let v = label("--", &["stat-value", "numeric"]);
    b.append(&v);
    (v, b)
}

/// The panel's status lamps, in the order a front panel would print them.
const LAMPS: [(&str, &str, Tone); 7] = [
    ("CV", "Regulating voltage", Tone::Ok),
    ("CC", "Limiting current", Tone::Warn),
    ("OUT", "Output on", Tone::Alert),
    ("OVP", "Over-voltage trip", Tone::Alert),
    ("OCP", "Over-current trip", Tone::Alert),
    ("OPP", "Over-power trip", Tone::Alert),
    ("OTP", "Over-temperature trip", Tone::Alert),
];

/// The time windows offered, in seconds.
const WINDOWS: [(&str, f64); 6] = [
    ("10 s", 10.0),
    ("30 s", 30.0),
    ("1 min", 60.0),
    ("5 min", 300.0),
    ("15 min", 900.0),
    ("1 h", 3600.0),
];

/// A row of toggle buttons drawn as one segmented control. Clicking one
/// doesn't take keyboard focus, so no focus ring is left behind; Tab still
/// reaches them.
fn segmented() -> gtk::Box {
    gtk::Box::builder().css_classes(["segmented"]).build()
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
fn graph_section(graph: &Graph) -> (gtk::Box, gtk::FlowBox, gtk::Box) {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    // The two halves wrap rather than hold one long row: a page whose
    // content can't get narrow is clamped wider than the others.
    let bar = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(2)
        .column_spacing(12)
        .row_spacing(8)
        .build();

    let windows = segmented();
    let mut first: Option<gtk::ToggleButton> = None;
    for (text, secs) in WINDOWS {
        let b = segment(text);
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
    super::panel::add_to_flow(&bar, &windows);

    // Lane toggles and pause stay together, so a narrow window can move
    // them under the time windows as one row.
    let right = gtk::Box::builder()
        .spacing(12)
        .hexpand(true)
        .halign(gtk::Align::End)
        .build();
    let lanes = segmented();
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
    super::panel::add_to_flow(&bar, &right);

    section.append(&bar);
    let frame = gtk::Box::builder()
        .css_classes(["card", "graph-card"])
        .build();
    frame.append(graph);
    section.append(&frame);
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

pub struct Readouts {
    pub root: Page,
    panel: Panel,
    channels: gtk::Box,
    graph_bar: gtk::FlowBox,
    lane_toggles: gtk::Box,
    controls: Rc<Controls>,
    graph: Graph,
    volts: Channel,
    amps: Channel,
    power: gtk::Label,
    vin: gtk::Label,
    temp: gtk::Label,
    protection: gtk::Label,
    preset: gtk::Label,
    delivered: gtk::Label,
    rail: gtk::Label,
    last_labels: Cell<Option<Instant>>,
    setpoints: Cell<Option<(u16, u16)>>,
}

impl Readouts {
    pub fn new(controls: Rc<Controls>) -> Self {
        // A preferences page like the others, so every page has the same
        // width and margins.
        let root = Page::new();
        let group = |w: &gtk::Widget| {
            let g = adw::PreferencesGroup::new();
            g.add(w);
            root.add(&g);
        };

        // --- the instrument panel ---
        let panel = Panel::new(&LAMPS, Some(controls.toggle.upcast_ref()));

        let channels = gtk::Box::builder().spacing(28).homogeneous(true).build();
        let (volts, b) = Channel::new("VOLTAGE", "V", "volt");
        channels.append(&b);
        let (amps, b) = Channel::new("CURRENT", "A", "amp");
        channels.append(&b);
        panel.body.append(&channels);

        let footer = super::panel::footer();
        footer.set_max_children_per_line(3);
        let (power, b) = stat("POWER");
        super::panel::add_to_flow(&footer, &b);
        let (vin, b) = stat("INPUT");
        super::panel::add_to_flow(&footer, &b);
        let (temp, b) = stat("TEMPERATURE");
        super::panel::add_to_flow(&footer, &b);
        panel.body.append(&footer);
        group(panel.root.upcast_ref());

        root.add(&controls.setpoint);

        let graph = Graph::default();
        if let Some(secs) = std::env::var("DP100_STUDIO_PREFILL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            graph.prefill(secs);
        }
        let (section, graph_bar, lane_toggles) = graph_section(&graph);
        group(section.upcast_ref());

        let output = adw::PreferencesGroup::builder().title("Details").build();
        let protection = row(&output, "Protection trips");
        let preset = row(&output, "Active preset");
        let delivered = row(&output, "Delivered since connecting");
        let rail = row(&output, "Internal 5 V rail");
        root.add(&output);

        Self {
            root,
            panel,
            channels: channels.clone(),
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
            preset,
            delivered,
            rail,
            last_labels: Cell::new(None),
            setpoints: Cell::new(None),
        }
    }

    /// On a narrow window, rows that would overflow are stacked: the power
    /// switch drops below the lamps at full width, and the lane toggles go
    /// under the time windows.
    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        let vertical = gtk::Orientation::Vertical.to_value();
        self.panel
            .narrow_layout(bp, Some(self.controls.toggle.upcast_ref()));
        bp.add_setter(&self.channels, "orientation", Some(&vertical));
        bp.add_setter(&self.channels, "spacing", Some(&18.to_value()));
        bp.add_setter(
            &self.graph_bar,
            "max-children-per-line",
            Some(&1u32.to_value()),
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

        self.volts
            .value
            .set_label(&format!("{:.3}", f64::from(m.vout_mv) / 1000.0));
        self.amps
            .value
            .set_label(&format!("{:.3}", f64::from(m.iout_ma) / 1000.0));
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
        self.lamp(2, on);
        self.lamp(3, m.fault == Fault::OverVoltage);
        self.lamp(4, m.fault == Fault::OverCurrent);
        self.lamp(5, m.fault == Fault::OverPower);
        self.lamp(6, m.fault == Fault::OverTemperature);
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
        self.preset.set_label(&if o.index == 0 {
            "0 (boot default)".to_owned()
        } else {
            o.index.to_string()
        });
    }

    pub fn show_energy(&self, mwh: f64, mah: f64) {
        self.delivered
            .set_label(&format!("{} · {}", energy(mwh), charge(mah)));
    }
}
