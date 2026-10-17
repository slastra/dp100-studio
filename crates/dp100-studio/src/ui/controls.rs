//! Setpoint editing and the output switch.
//!
//! The spin rows follow the device until the user edits them; from then
//! until Apply (or the device changing underneath), they hold the user's
//! values. Turning the output on always asks first and says what it will
//! supply; turning it off never asks.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode, Setpoints};

use super::ctx::{Ctx, amps, milli, volts};
use crate::worker::Action;

pub struct Controls {
    pub setpoint: adw::PreferencesGroup,
    pub toggle: gtk::Button,
    v: adw::SpinRow,
    i: adw::SpinRow,
    apply: gtk::Button,
    /// The rows are being set from the device, not by the user.
    syncing: Cell<bool>,
    /// The user has changed a value that has not been applied.
    dirty: Cell<bool>,
    on: Cell<bool>,
    ctx: Rc<Ctx>,
}

impl Controls {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let v = adw::SpinRow::with_range(0.0, 30.0, 0.01);
        v.set_title("Voltage");
        v.set_digits(2);
        let i = adw::SpinRow::with_range(0.0, 5.0, 0.001);
        i.set_title("Current limit");
        i.set_digits(3);

        let apply = gtk::Button::builder()
            .label("Apply")
            .css_classes(["suggested-action"])
            .valign(gtk::Align::Center)
            .sensitive(false)
            .tooltip_text("Send the new setpoints. The output stays on or off as it is.")
            .build();
        let setpoint = adw::PreferencesGroup::builder()
            .title("Setpoint")
            .header_suffix(&apply)
            .build();
        setpoint.add(&v);
        setpoint.add(&i);

        let toggle = gtk::Button::builder()
            .label("Turn Output On")
            .css_classes(["pill", "suggested-action"])
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .hexpand(true)
            .build();

        let c = Rc::new(Self {
            setpoint,
            toggle,
            v,
            i,
            apply,
            syncing: Cell::new(false),
            dirty: Cell::new(false),
            on: Cell::new(false),
            ctx,
        });

        for row in [&c.v, &c.i] {
            let w = Rc::downgrade(&c);
            row.connect_value_notify(move |_| {
                if let Some(c) = w.upgrade()
                    && !c.syncing.get()
                {
                    c.dirty.set(true);
                    c.apply.set_sensitive(true);
                }
            });
        }

        let w = Rc::downgrade(&c);
        c.apply.connect_clicked(move |_| {
            if let Some(c) = w.upgrade() {
                c.ctx.act(Action::SetSetpoint {
                    vset_mv: milli(c.v.value()),
                    iset_ma: milli(c.i.value()),
                });
                c.dirty.set(false);
                c.apply.set_sensitive(false);
            }
        });

        let w = Rc::downgrade(&c);
        c.toggle.connect_clicked(move |_| {
            if let Some(c) = w.upgrade() {
                c.toggle_output();
            }
        });
        c
    }

    fn toggle_output(&self) {
        if self.on.get() {
            self.ctx.act(Action::SetOutput(false));
            return;
        }
        let Some(o) = self.ctx.live.borrow().output else {
            return;
        };
        let mut body = format!(
            "The DP100 will supply {} with a current limit of {} (preset {}).",
            volts(o.vset_mv),
            amps(o.iset_ma),
            o.index
        );
        if self.dirty.get() {
            body.push_str("\n\nYour unapplied setpoint changes are not included. Apply them first to use them.");
        }
        let ctx = self.ctx.clone();
        self.ctx.confirm(
            "Turn on the output?",
            &body,
            "Turn On",
            adw::ResponseAppearance::Suggested,
            move || ctx.act(Action::SetOutput(true)),
        );
    }

    pub fn show_output(&self, o: &Setpoints) {
        self.syncing.set(true);
        // The rows can't go past the protection limit: the firmware would
        // trip the moment the output came on. (A new upper bound can clamp
        // the value, which must not count as the user editing it.)
        self.v
            .adjustment()
            .set_upper(f64::from(o.ovp_mv.min(30_000)) / 1000.0);
        self.i
            .adjustment()
            .set_upper(f64::from(o.ocp_ma.min(5_000)) / 1000.0);
        if !self.dirty.get() {
            self.v.set_value(f64::from(o.vset_mv) / 1000.0);
            self.i.set_value(f64::from(o.iset_ma) / 1000.0);
        }
        self.syncing.set(false);
    }

    pub fn show_sample(&self, m: &BasicInfo) {
        let on = m.output_on().unwrap_or(false);
        if on != self.on.get() {
            self.on.set(on);
            if on {
                self.toggle.set_label("Turn Output Off");
                self.toggle.remove_css_class("suggested-action");
                self.toggle.add_css_class("destructive-action");
            } else {
                self.toggle.set_label("Turn Output On");
                self.toggle.remove_css_class("destructive-action");
                self.toggle.add_css_class("suggested-action");
            }
        }
        // Off always works. On needs input power and no latched trip.
        self.toggle
            .set_sensitive(on || (m.mode != Mode::NoInput && m.fault == Fault::Ok));
        self.v.set_subtitle(&format!(
            "Up to {} available from this input",
            volts(m.vo_max_mv)
        ));
    }
}
