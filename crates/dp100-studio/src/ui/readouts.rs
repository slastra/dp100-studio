//! The live readout panel: big output figures and the details beneath.

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode, Setpoints, SystemSettings};

fn volts(mv: u16) -> String {
    format!("{:.3} V", f64::from(mv) / 1000.0)
}

fn amps(ma: u16) -> String {
    format!("{:.3} A", f64::from(ma) / 1000.0)
}

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

pub struct Readouts {
    pub root: gtk::Box,
    vout: Figure,
    iout: Figure,
    power: Figure,
    mode: gtk::Label,
    setpoint: gtk::Label,
    protection: gtk::Label,
    preset: gtk::Label,
    vin: gtk::Label,
    vo_max: gtk::Label,
    temps: gtk::Label,
    rail: gtk::Label,
    limits: gtk::Label,
}

impl Readouts {
    pub fn new() -> Self {
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
            .build();
        root.append(&mode);

        let output = adw::PreferencesGroup::builder().title("Output").build();
        let setpoint = row(&output, "Setpoint");
        let protection = row(&output, "Protection");
        let preset = row(&output, "Preset");
        root.append(&output);

        let supply = adw::PreferencesGroup::builder().title("Supply").build();
        let vin = row(&supply, "Input");
        let vo_max = row(&supply, "Highest output available");
        let temps = row(&supply, "Temperature");
        let rail = row(&supply, "Internal 5 V rail");
        let limits = row(&supply, "Trip limits");
        root.append(&supply);

        Self {
            root,
            vout,
            iout,
            power,
            mode,
            setpoint,
            protection,
            preset,
            vin,
            vo_max,
            temps,
            rail,
            limits,
        }
    }

    pub fn show_sample(&self, m: &BasicInfo) {
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
        self.setpoint
            .set_label(&format!("{} · {}", volts(o.vset_mv), amps(o.iset_ma)));
        self.protection
            .set_label(&format!("OVP {} · OCP {}", volts(o.ovp_mv), amps(o.ocp_ma)));
        self.preset.set_label(&if o.index == 0 {
            "0 (boot default)".to_owned()
        } else {
            o.index.to_string()
        });
    }

    pub fn show_system(&self, s: &SystemSettings) {
        self.limits.set_label(&format!(
            "OTP {} °C · OPP {:.1} W",
            s.otp_c,
            f64::from(s.opp_dw) / 10.0
        ));
    }
}
