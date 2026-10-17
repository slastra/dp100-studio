//! Setpoint editing and the output switch.
//!
//! The setpoints apply live, like turning a knob on the supply itself:
//! once the user stops adjusting for a moment, the new values are sent.
//! Holding a + button would otherwise queue a write per step, each of which
//! takes about 120 ms to verify. While an edit is on its way, the rows keep
//! the user's values instead of snapping back to the device's older ones.
//!
//! Turning the output on always asks first and says what it will supply;
//! turning it off never asks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode, Setpoints};
use gtk::glib;

use super::ctx::{Ctx, amps, milli, volts};
use crate::worker::Action;

/// Quiet time after the last adjustment before the setpoints are sent.
const SETTLE: Duration = Duration::from_millis(250);
/// How long a sent edit may take to show up in the device's readings
/// before the rows go back to following the device.
const PENDING_FOR: Duration = Duration::from_secs(2);

pub struct Controls {
    pub setpoint: adw::PreferencesGroup,
    /// The power button, placed in the readout panel by its owner.
    pub toggle: gtk::Button,
    toggle_content: adw::ButtonContent,
    v: adw::SpinRow,
    i: adw::SpinRow,
    /// The rows are being set from the device, not by the user.
    syncing: Cell<bool>,
    /// Waiting for the user to stop adjusting.
    timer: RefCell<Option<glib::SourceId>>,
    /// Sent, not yet seen back from the device: (mV, mA) and when.
    pending: Cell<Option<((u16, u16), Instant)>>,
    on: Cell<bool>,
    ctx: Rc<Ctx>,
}

impl Controls {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let v = adw::SpinRow::with_range(0.0, 30.0, 0.01);
        v.set_title("Voltage");
        v.set_digits(2);
        v.add_css_class("numeric");
        let i = adw::SpinRow::with_range(0.0, 5.0, 0.001);
        i.set_title("Current limit");
        i.set_digits(3);
        i.add_css_class("numeric");

        let setpoint = adw::PreferencesGroup::builder()
            .title("Setpoint")
            .description("Changes apply as you make them; the output stays on or off as it is")
            .build();
        setpoint.add(&v);
        setpoint.add(&i);

        let toggle_content = adw::ButtonContent::builder()
            .icon_name("system-shutdown-symbolic")
            .label("Turn On")
            .build();
        let toggle = gtk::Button::builder()
            .child(&toggle_content)
            .css_classes(["pill", "power-button"])
            .valign(gtk::Align::Center)
            .tooltip_text("Turn the Output On")
            .build();

        let c = Rc::new(Self {
            setpoint,
            toggle,
            toggle_content,
            v,
            i,
            syncing: Cell::new(false),
            timer: RefCell::new(None),
            pending: Cell::new(None),
            on: Cell::new(false),
            ctx,
        });

        for row in [&c.v, &c.i] {
            let w = Rc::downgrade(&c);
            row.connect_value_notify(move |_| {
                if let Some(c) = w.upgrade()
                    && !c.syncing.get()
                {
                    c.schedule();
                }
            });
        }

        let w = Rc::downgrade(&c);
        c.toggle.connect_clicked(move |_| {
            if let Some(c) = w.upgrade() {
                c.toggle_output();
            }
        });
        c
    }

    /// (Re)starts the quiet-time timer after an adjustment.
    fn schedule(self: &Rc<Self>) {
        if let Some(id) = self.timer.take() {
            id.remove();
        }
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(SETTLE, move || {
            if let Some(c) = w.upgrade() {
                c.timer.take();
                c.send();
            }
        });
        self.timer.replace(Some(id));
    }

    /// Sends what the rows say now, cancelling any wait.
    fn send(&self) {
        if let Some(id) = self.timer.take() {
            id.remove();
        }
        let want = (milli(self.v.value()), milli(self.i.value()));
        self.pending.set(Some((want, Instant::now())));
        self.ctx.act(Action::SetSetpoint {
            vset_mv: want.0,
            iset_ma: want.1,
        });
    }

    /// While a sequence runs it owns the setpoints.
    pub fn set_locked(&self, locked: bool) {
        self.v.set_sensitive(!locked);
        self.i.set_sensitive(!locked);
    }

    fn toggle_output(&self) {
        if self.on.get() {
            self.ctx.act(Action::SetOutput(false));
            return;
        }
        // An adjustment still waiting to go out goes first, so the output
        // comes on at what the rows show.
        if self.timer.borrow().is_some() {
            self.send();
        }
        let Some(o) = self.ctx.live.borrow().output else {
            return;
        };
        let (vset, iset) = self
            .pending
            .get()
            .map_or((o.vset_mv, o.iset_ma), |(p, _)| p);
        let body = format!(
            "The DP100 will supply {} with a current limit of {} (preset {}).",
            volts(vset),
            amps(iset),
            o.index
        );
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
        let follow = if self.timer.borrow().is_some() {
            // Mid-adjustment: the user's values win.
            false
        } else {
            match self.pending.get() {
                None => true,
                Some((want, since)) => {
                    // Follow the device again once it shows the edit, or if
                    // it never does (the write was refused, or replaced).
                    let done = want == (o.vset_mv, o.iset_ma) || since.elapsed() > PENDING_FOR;
                    if done {
                        self.pending.set(None);
                    }
                    done
                }
            }
        };
        if follow {
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
                self.toggle_content.set_label("Turn Off");
                self.toggle.add_css_class("on");
                self.toggle
                    .set_tooltip_text(Some("Turn the Output Off (F9)"));
            } else {
                self.toggle_content.set_label("Turn On");
                self.toggle.remove_css_class("on");
                self.toggle.set_tooltip_text(Some("Turn the Output On"));
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
