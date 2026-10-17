//! The Device page: protection trips, panel and safety settings, and what
//! the unit is.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::{BasicInfo, DeviceInfo, Setpoints, SystemSettings};
use gtk::glib;

use super::ctx::{Ctx, milli, volts};
use super::panel::{Panel, Tone, figure, footer};
use crate::worker::Action;

/// Rows that follow the device until edited. Apply and Revert appear only
/// once something has changed: these settings persist on the DP100, so
/// they are sent deliberately rather than live.
struct Form {
    apply: gtk::Button,
    revert: gtk::Button,
    bar: gtk::Revealer,
    syncing: Cell<bool>,
    dirty: Cell<bool>,
}

impl Form {
    fn new(tooltip: &str) -> Rc<Self> {
        let apply = gtk::Button::builder()
            .label("Apply")
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .tooltip_text(tooltip)
            .build();
        let revert = gtk::Button::builder()
            .label("Revert")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Discard Your Changes")
            .build();
        let buttons = gtk::Box::builder().spacing(6).build();
        buttons.append(&revert);
        buttons.append(&apply);
        let bar = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .child(&buttons)
            .build();
        Rc::new(Self {
            apply,
            revert,
            bar,
            syncing: Cell::new(false),
            dirty: Cell::new(false),
        })
    }

    fn watch(self: &Rc<Self>, w: &impl IsA<glib::Object>, property: &str) {
        let f = Rc::downgrade(self);
        w.as_ref()
            .connect_notify_local(Some(property), move |_, _| {
                if let Some(f) = f.upgrade()
                    && !f.syncing.get()
                {
                    f.dirty.set(true);
                    f.bar.set_reveal_child(true);
                }
            });
    }

    /// Runs `set` as a device update, unless the user has unapplied edits.
    fn sync(&self, set: impl FnOnce()) {
        if self.dirty.get() {
            return;
        }
        self.syncing.set(true);
        set();
        self.syncing.set(false);
    }

    fn applied(&self) {
        self.dirty.set(false);
        self.bar.set_reveal_child(false);
    }
}

fn spin(title: &str, subtitle: &str, min: f64, max: f64, step: f64, digits: u32) -> adw::SpinRow {
    let r = adw::SpinRow::with_range(min, max, step);
    r.set_title(title);
    if !subtitle.is_empty() {
        r.set_subtitle(subtitle);
    }
    r.set_digits(digits);
    r
}

pub struct Device {
    pub root: adw::PreferencesPage,
    protection: Rc<Form>,
    ovp: adw::SpinRow,
    ocp: adw::SpinRow,
    system: Rc<Form>,
    otp: adw::SpinRow,
    opp: adw::SpinRow,
    backlight: adw::SpinRow,
    volume: adw::SpinRow,
    rpp: adw::SwitchRow,
    auto_on: adw::SwitchRow,
    panel: Panel,
    release: gtk::Button,
    title: gtk::Label,
    meta: gtk::Label,
    vin: gtk::Label,
    vo_max: gtk::Label,
    temp: gtk::Label,
    rail: gtk::Label,
    simulated: bool,
    ctx: Rc<Ctx>,
}

/// Panel lamps: the panel lock while this app holds the DP100, and the two
/// settings that change how the supply behaves on its own.
const LAMPS: [(&str, &str, Tone); 3] = [
    (
        "PANEL LOCK",
        "The DP100's buttons are locked while DP100 Studio is connected",
        Tone::Info,
    ),
    ("RPP", "Reverse-polarity protection is on", Tone::Ok),
    (
        "AUTO-ON",
        "The output comes on by itself at power-up",
        Tone::Warn,
    ),
];

impl Device {
    pub fn new(ctx: Rc<Ctx>, simulated: bool) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        // --- the unit, as a panel ---
        let release = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("changes-allow-symbolic")
                    .label("Release Panel")
                    .build(),
            )
            .css_classes(["pill", "power-button"])
            .valign(gtk::Align::Center)
            .tooltip_text("Unlock the DP100's Buttons Until You Reconnect")
            .action_name("win.release")
            .build();
        let panel = Panel::new(&LAMPS, Some(release.upcast_ref()));
        let title = gtk::Label::builder()
            .label("DP100")
            .css_classes(["seq-title"])
            .xalign(0.0)
            .build();
        let meta = gtk::Label::builder()
            .css_classes(["channel-note"])
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        head.append(&title);
        head.append(&meta);
        panel.body.append(&head);
        let foot = footer();
        let (vin, b) = figure("INPUT", "plain");
        foot.append(&b);
        let (vo_max, b) = figure("AVAILABLE", "volt");
        foot.append(&b);
        let (temp, b) = figure("TEMPERATURE", "heat");
        foot.append(&b);
        let (rail, b) = figure("5 V RAIL", "plain");
        foot.append(&b);
        foot.add_css_class("compact");
        panel.body.append(&foot);
        let hg = adw::PreferencesGroup::new();
        hg.add(&panel.root);
        root.add(&hg);

        let protection = Form::new("Save the Trips to the Active Preset");
        let pg = adw::PreferencesGroup::builder()
            .title("Protection")
            .description(
                "Trips for the active preset. The output shuts off if it goes past \
                 them. Saved to the preset and applied at once.",
            )
            .header_suffix(&protection.bar)
            .build();
        let ovp = spin("Over-voltage trip (V)", "", 0.0, 30.5, 0.01, 2);
        let ocp = spin("Over-current trip (A)", "", 0.0, 5.05, 0.001, 3);
        pg.add(&ovp);
        pg.add(&ocp);
        root.add(&pg);

        let system = Form::new("Save These Settings to the DP100");
        let sg = adw::PreferencesGroup::builder()
            .title("Settings")
            .header_suffix(&system.bar)
            .build();
        let otp = spin("Over-temperature trip (°C)", "", 50.0, 80.0, 1.0, 0);
        let opp = spin("Over-power trip (W)", "", 1.0, 105.0, 0.1, 1);
        let backlight = spin("Backlight", "", 0.0, 4.0, 1.0, 0);
        let volume = spin("Volume", "", 0.0, 4.0, 1.0, 0);
        let rpp = adw::SwitchRow::builder()
            .title("Reverse-polarity protection")
            .build();
        let auto_on = adw::SwitchRow::builder()
            .title("Output on at power-up")
            .subtitle("The output turns on by itself whenever the DP100 starts")
            .build();
        for r in [&otp, &opp, &backlight, &volume] {
            sg.add(r);
        }
        sg.add(&rpp);
        sg.add(&auto_on);
        root.add(&sg);

        for r in [&ovp, &ocp] {
            protection.watch(r, "value");
        }
        for r in [&otp, &opp, &backlight, &volume] {
            system.watch(r, "value");
        }
        for r in [&rpp, &auto_on] {
            system.watch(r, "active");
        }

        let d = Rc::new(Self {
            root,
            protection,
            ovp,
            ocp,
            system,
            otp,
            opp,
            backlight,
            volume,
            rpp,
            auto_on,
            panel,
            release,
            title,
            meta,
            vin,
            vo_max,
            temp,
            rail,
            simulated,
            ctx,
        });

        let w = Rc::downgrade(&d);
        d.protection.apply.connect_clicked(move |_| {
            if let Some(d) = w.upgrade() {
                d.apply_protection();
            }
        });
        let w = Rc::downgrade(&d);
        d.protection.revert.connect_clicked(move |_| {
            if let Some(d) = w.upgrade() {
                d.protection.applied();
                let o = d.ctx.live.borrow().output;
                if let Some(o) = o {
                    d.show_output(&o);
                }
            }
        });
        let w = Rc::downgrade(&d);
        d.system.revert.connect_clicked(move |_| {
            if let Some(d) = w.upgrade() {
                d.system.applied();
                let s = d.ctx.live.borrow().system;
                if let Some(s) = s {
                    d.show_system(&s);
                }
            }
        });
        let w = Rc::downgrade(&d);
        d.system.apply.connect_clicked(move |_| {
            if let Some(d) = w.upgrade() {
                d.apply_system();
            }
        });
        d
    }

    fn apply_protection(&self) {
        let (ovp_mv, ocp_ma) = (milli(self.ovp.value()), milli(self.ocp.value()));
        if let Some(o) = self.ctx.live.borrow().output
            && (ovp_mv < o.vset_mv || ocp_ma < o.iset_ma)
        {
            self.ctx.error(
                "The trips must be at or above the setpoints, or the output trips as soon as it is on.",
            );
            return;
        }
        self.ctx.act(Action::SetProtection { ovp_mv, ocp_ma });
        self.protection.applied();
    }

    fn apply_system(&self) {
        let Some(cur) = self.ctx.live.borrow().system else {
            return;
        };
        let want = SystemSettings {
            otp_c: self.otp.value().round() as u16,
            opp_dw: (self.opp.value() * 10.0).round() as u16,
            backlight: self.backlight.value().round() as u8,
            volume: self.volume.value().round() as u8,
            reverse_protect: cur.reverse_protect.map(|_| self.rpp.is_active()),
            auto_on: cur.auto_on.map(|_| self.auto_on.is_active()),
        };
        let ctx = self.ctx.clone();
        let send = move || ctx.act(Action::SetSystem(want));
        if want.auto_on == Some(true) && cur.auto_on != Some(true) {
            self.ctx.confirm(
                "Turn the output on at power-up?",
                "Whenever the DP100 starts, its output will come on by itself with the \
                 boot preset's setpoints, before any software is connected.",
                "Turn On at Power-Up",
                adw::ResponseAppearance::Destructive,
                send,
            );
        } else {
            send();
        }
        self.system.applied();
    }

    pub fn show_info(&self, i: &DeviceInfo) {
        self.title.set_label(&if self.simulated {
            format!("{} (simulated)", i.model)
        } else {
            i.model.clone()
        });
        let (y, m, d) = i.manufactured;
        self.meta.set_label(&format!(
            "Firmware {} · hardware {} · bootloader {} · made {y:04}-{m:02}-{d:02} · serial {}",
            i.firmware,
            i.hardware,
            i.bootloader,
            i.serial_hex()
        ));
        // Connected means the panel is locked; Release Panel undoes that.
        self.panel.lamp(0, true);
    }

    pub fn show_sample(&self, m: &BasicInfo) {
        self.vin.set_label(&volts(m.vin_mv));
        self.vo_max.set_label(&volts(m.vo_max_mv));
        self.temp.set_label(&format!(
            "{:.1}\u{202f}°C",
            f64::from(m.temp1_dc.max(m.temp2_dc)) / 10.0
        ));
        self.rail.set_label(&volts(m.dc5v_mv));
    }

    pub fn narrow_layout(&self, bp: &adw::Breakpoint) {
        self.panel
            .narrow_layout(bp, Some(self.release.upcast_ref()));
    }

    pub fn show_output(&self, o: &Setpoints) {
        self.protection.sync(|| {
            self.ovp.set_value(f64::from(o.ovp_mv) / 1000.0);
            self.ocp.set_value(f64::from(o.ocp_ma) / 1000.0);
        });
    }

    pub fn show_system(&self, s: &SystemSettings) {
        self.system.sync(|| {
            self.otp.set_value(f64::from(s.otp_c));
            self.opp.set_value(f64::from(s.opp_dw) / 10.0);
            self.backlight.set_value(f64::from(s.backlight));
            self.volume.set_value(f64::from(s.volume));
            self.rpp.set_visible(s.reverse_protect.is_some());
            self.auto_on.set_visible(s.auto_on.is_some());
            self.rpp.set_active(s.reverse_protect.unwrap_or(false));
            self.auto_on.set_active(s.auto_on.unwrap_or(false));
        });
        self.panel.lamp(1, s.reverse_protect == Some(true));
        self.panel.lamp(2, s.auto_on == Some(true));
    }
}
