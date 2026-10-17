//! The Device page: protection trips, panel and safety settings, and what
//! the unit is.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::{DeviceInfo, Setpoints, SystemSettings};
use gtk::glib;

use super::ctx::{Ctx, milli};
use crate::worker::Action;

/// Rows that follow the device until edited, with an Apply button that
/// lights up on the first edit.
struct Form {
    apply: gtk::Button,
    syncing: Cell<bool>,
    dirty: Cell<bool>,
}

impl Form {
    fn new(tooltip: &str) -> Rc<Self> {
        Rc::new(Self {
            apply: gtk::Button::builder()
                .label("Apply")
                .css_classes(["suggested-action"])
                .valign(gtk::Align::Center)
                .sensitive(false)
                .tooltip_text(tooltip)
                .build(),
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
                    f.apply.set_sensitive(true);
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
        self.apply.set_sensitive(false);
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

fn info_row(group: &adw::PreferencesGroup, title: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build();
    group.add(&r);
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
    model: adw::ActionRow,
    firmware: adw::ActionRow,
    serial: adw::ActionRow,
    made: adw::ActionRow,
    ctx: Rc<Ctx>,
}

impl Device {
    pub fn new(ctx: Rc<Ctx>, simulated: bool) -> Rc<Self> {
        let root = adw::PreferencesPage::new();

        let protection = Form::new("Save the trips to the active preset. They apply at once.");
        let pg = adw::PreferencesGroup::builder()
            .title("Protection")
            .description(
                "Trips for the active preset. The output shuts off if it goes past \
                 them. Saved to the preset and applied at once.",
            )
            .header_suffix(&protection.apply)
            .build();
        let ovp = spin("Over-voltage trip (V)", "", 0.0, 30.5, 0.01, 2);
        let ocp = spin("Over-current trip (A)", "", 0.0, 5.05, 0.001, 3);
        pg.add(&ovp);
        pg.add(&ocp);
        root.add(&pg);

        let system = Form::new("Save these settings to the DP100.");
        let sg = adw::PreferencesGroup::builder()
            .title("Settings")
            .header_suffix(&system.apply)
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

        let ig = adw::PreferencesGroup::builder()
            .title("About This Unit")
            .build();
        let model = info_row(&ig, "Model");
        if simulated {
            model.set_subtitle("ATK-DP100 (simulated)");
        }
        let firmware = info_row(&ig, "Firmware");
        let serial = info_row(&ig, "Serial");
        let made = info_row(&ig, "Manufactured");
        root.add(&ig);

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
            model,
            firmware,
            serial,
            made,
            ctx,
        });

        let w = Rc::downgrade(&d);
        d.protection.apply.connect_clicked(move |_| {
            if let Some(d) = w.upgrade() {
                d.apply_protection();
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
        if self.model.subtitle().is_none_or(|s| s.is_empty()) {
            self.model.set_subtitle(&i.model);
        }
        self.firmware.set_subtitle(&format!(
            "{} (hardware {}, bootloader {})",
            i.firmware, i.hardware, i.bootloader
        ));
        self.serial.set_subtitle(&i.serial_hex());
        let (y, m, d) = i.manufactured;
        self.made.set_subtitle(&format!("{y:04}-{m:02}-{d:02}"));
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
    }
}
