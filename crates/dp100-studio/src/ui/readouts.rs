//! The Monitor page: big output figures, the controls, the graph, and the
//! details beneath.

use std::rc::Rc;
use std::time::Instant;

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode, Setpoints};

use super::controls::Controls;
use super::ctx::{amps, volts};
use super::graph::{Graph, Lane};

fn celsius(dc: i16) -> String {
    format!("{:.1} °C", f64::from(dc) / 10.0)
}

/// One large figure with a caption.
struct Figure {
    value: gtk::Label,
}

impl Figure {
    fn new(caption: &str, class: &str) -> (Self, gtk::Box) {
        let b = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .css_classes(["card", "figure", class])
            .build();
        let value = gtk::Label::builder()
            .label("--")
            .css_classes(["figure-value", "numeric"])
            .xalign(0.0)
            .build();
        let cap = gtk::Label::builder()
            .label(caption)
            .css_classes(["caption", "dim-label"])
            .xalign(0.0)
            .build();
        b.append(&cap);
        b.append(&value);
        (Self { value }, b)
    }
}

/// An `AdwActionRow` whose suffix label is updated in place.
fn row(group: &adw::PreferencesGroup, title: &str) -> gtk::Label {
    let value = gtk::Label::builder()
        .css_classes(["numeric"])
        .selectable(true)
        .build();
    let r = adw::ActionRow::builder().title(title).build();
    r.add_suffix(&value);
    group.add(&r);
    value
}

/// The time windows offered, in seconds.
const WINDOWS: [(&str, f64); 6] = [
    ("10 s", 10.0),
    ("30 s", 30.0),
    ("1 min", 60.0),
    ("5 min", 300.0),
    ("15 min", 900.0),
    ("1 h", 3600.0),
];

/// The graph with its window, lane and pause controls above it.
fn graph_section(graph: &Graph) -> gtk::Box {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let bar = gtk::Box::builder().spacing(12).build();

    let windows = gtk::Box::builder().css_classes(["linked"]).build();
    let mut first: Option<gtk::ToggleButton> = None;
    for (label, secs) in WINDOWS {
        let b = gtk::ToggleButton::builder()
            .label(label)
            .css_classes(["flat", "small"])
            .active(secs == 30.0)
            .build();
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
    bar.append(&windows);

    let lanes = gtk::Box::builder()
        .css_classes(["linked"])
        .hexpand(true)
        .halign(gtk::Align::End)
        .build();
    for lane in Lane::ALL {
        let b = gtk::ToggleButton::builder()
            .label(lane.short())
            .tooltip_text(format!("Show {}", lane.name().to_lowercase()))
            .css_classes(["flat", "small", "lane-toggle"])
            .active(graph.lane_shown(lane))
            .build();
        let g = graph.clone();
        b.connect_toggled(move |b| g.set_lane(lane, b.is_active()));
        lanes.append(&b);
    }
    bar.append(&lanes);

    let pause = gtk::ToggleButton::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text("Pause the graph (recording continues)")
        .css_classes(["flat"])
        .build();
    let g = graph.clone();
    pause.connect_toggled(move |b| g.set_paused(b.is_active()));
    bar.append(&pause);

    section.append(&bar);
    let frame = gtk::Box::builder()
        .css_classes(["card", "graph-card"])
        .build();
    frame.append(graph);
    section.append(&frame);
    section
}

pub struct Readouts {
    pub root: gtk::Box,
    controls: Rc<Controls>,
    graph: Graph,
    vout: Figure,
    iout: Figure,
    power: Figure,
    mode: gtk::Label,
    protection: gtk::Label,
    preset: gtk::Label,
    vin: gtk::Label,
    vo_max: gtk::Label,
    temps: gtk::Label,
    rail: gtk::Label,
}

impl Readouts {
    pub fn new(controls: Rc<Controls>) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .build();

        let figures = gtk::Box::builder().spacing(12).homogeneous(true).build();
        let (vout, b) = Figure::new("Voltage", "volts");
        figures.append(&b);
        let (iout, b) = Figure::new("Current", "amps");
        figures.append(&b);
        let (power, b) = Figure::new("Power", "watts");
        figures.append(&b);
        root.append(&figures);

        let mode = gtk::Label::builder()
            .css_classes(["mode-badge"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Center)
            .build();
        let state = gtk::Box::builder().spacing(12).build();
        state.append(&mode);
        state.append(&controls.toggle);
        root.append(&state);
        root.append(&controls.setpoint);

        let graph = Graph::default();
        if let Some(secs) = std::env::var("DP100_STUDIO_PREFILL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            graph.prefill(secs);
        }
        root.append(&graph_section(&graph));

        let output = adw::PreferencesGroup::builder().title("Output").build();
        let protection = row(&output, "Protection");
        let preset = row(&output, "Preset");
        root.append(&output);

        let supply = adw::PreferencesGroup::builder().title("Supply").build();
        let vin = row(&supply, "Input");
        let vo_max = row(&supply, "Highest output available");
        let temps = row(&supply, "Temperature");
        let rail = row(&supply, "Internal 5 V rail");
        root.append(&supply);

        Self {
            root,
            controls,
            graph,
            vout,
            iout,
            power,
            mode,
            protection,
            preset,
            vin,
            vo_max,
            temps,
            rail,
        }
    }

    pub fn show_sample(&self, at: Instant, m: &BasicInfo) {
        self.graph.push(at, m);
        self.controls.show_sample(m);
        self.vout.value.set_label(&volts(m.vout_mv));
        self.iout.value.set_label(&amps(m.iout_ma));
        self.power
            .value
            .set_label(&format!("{:.3} W", f64::from(m.power_mw()) / 1000.0));
        self.vin.set_label(&volts(m.vin_mv));
        self.vo_max.set_label(&volts(m.vo_max_mv));
        self.temps.set_label(&format!(
            "{} / {}",
            celsius(m.temp1_dc),
            celsius(m.temp2_dc)
        ));
        self.rail.set_label(&volts(m.dc5v_mv));

        let (text, class) = match (m.fault, m.mode) {
            (f, _) if f != Fault::Ok => (format!("Tripped: {}", f.label()), "fault"),
            (_, Mode::ConstantVoltage) => ("Output on · constant voltage".into(), "cv"),
            (_, Mode::ConstantCurrent) => ("Output on · constant current".into(), "cc"),
            (_, Mode::NoInput) => ("No input power".into(), "fault"),
            (_, Mode::Off) => ("Output off".into(), "off"),
            (_, Mode::Unknown(b)) => (format!("Unknown mode {b:#04x}"), "off"),
        };
        self.mode.set_label(&text);
        for c in ["fault", "cv", "cc", "off"] {
            self.mode.remove_css_class(c);
        }
        self.mode.add_css_class(class);
    }

    pub fn show_output(&self, o: &Setpoints) {
        self.controls.show_output(o);
        self.protection
            .set_label(&format!("OVP {} · OCP {}", volts(o.ovp_mv), amps(o.ocp_ma)));
        self.preset.set_label(&if o.index == 0 {
            "0 (boot default)".to_owned()
        } else {
            o.index.to_string()
        });
    }
}
