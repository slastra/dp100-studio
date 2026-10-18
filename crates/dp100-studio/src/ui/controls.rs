//! Setpoint editing, the output switch, and power mode.
//!
//! The setpoints apply live, like turning a knob on the supply itself:
//! once the user stops adjusting for a moment, the new values are sent.
//! Holding a + button would otherwise queue a write per step, each of which
//! takes about 120 ms to verify. While an edit is on its way, the rows keep
//! the user's values instead of snapping back to the device's older ones.
//!
//! In power mode the rows set a power and the limits it stays inside, and
//! the worker moves the current limit to hold it (see `seq::power`). The
//! rows then show what was asked for, not the device's live setpoints,
//! which the readouts above still show.
//!
//! Turning the output on always asks first and says what it will supply;
//! turning it off never asks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use dp100::{BasicInfo, Fault, Mode as DeviceMode, Setpoints};
use gtk::glib;

use super::ctx::{Ctx, amps, milli, volts};
use crate::seq::power::{HoldState, PowerTarget};
use crate::worker::{Action, Command, PowerStatus};

/// Quiet time after the last adjustment before the setpoints are sent.
const SETTLE: Duration = Duration::from_millis(250);
/// How long a sent edit may take to show up in the device's readings
/// before the rows go back to following the device.
const PENDING_FOR: Duration = Duration::from_secs(2);

const VOLTAGE_NOTE: &str = "Changes apply as you make them";
const POWER_NOTE: &str = "Holds a power by moving the current limit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Voltage,
    Power,
}

pub struct Controls {
    pub setpoint: adw::PreferencesGroup,
    /// The first row: the mode, its note, and the switch between them.
    mode_row: adw::ActionRow,
    /// What the output is doing, in words, at the top of the group.
    status: adw::ActionRow,
    status_icon: gtk::Image,
    status_spinner: gtk::Spinner,
    /// The latest reading, setpoints and hold, for the status row.
    sample: Cell<Option<BasicInfo>>,
    output: Cell<Option<(u16, u16)>>,
    power: Cell<Option<PowerStatus>>,
    locked: Cell<bool>,
    /// The power button, placed in the readout panel by its owner.
    pub toggle: gtk::Button,
    toggle_content: adw::ButtonContent,
    by_voltage: gtk::ToggleButton,
    by_power: gtk::ToggleButton,
    p: adw::SpinRow,
    v: adw::SpinRow,
    i: adw::SpinRow,
    /// The rows are being set by the program, not by the user.
    syncing: Cell<bool>,
    /// Waiting for the user to stop adjusting.
    timer: RefCell<Option<glib::SourceId>>,
    /// Sent, not yet seen back from the device: (mV, mA) and when.
    pending: Cell<Option<((u16, u16), Instant)>>,
    on: Cell<bool>,
    mode: Cell<Mode>,
    /// The worker is holding power.
    holding: Cell<bool>,
    /// A hold was asked for and not yet confirmed or refused.
    starting: Cell<bool>,
    /// A hold ended: the next sample decides whether to stay in power mode.
    ended: Cell<bool>,
    /// The latest measured power, in watts.
    measured: Cell<f64>,
    ctx: Rc<Ctx>,
}

impl Controls {
    pub fn new(ctx: Rc<Ctx>) -> Rc<Self> {
        let p = adw::SpinRow::with_range(0.01, 100.0, 0.01);
        p.set_title("Power");
        p.set_digits(2);
        p.set_value(1.0);
        p.add_css_class("numeric");
        p.set_visible(false);
        let v = adw::SpinRow::with_range(0.0, 30.0, 0.01);
        v.set_title("Voltage");
        v.set_digits(2);
        v.add_css_class("numeric");
        let i = adw::SpinRow::with_range(0.0, 5.0, 0.001);
        i.set_title("Current limit");
        i.set_digits(3);
        i.add_css_class("numeric");

        let by_voltage = gtk::ToggleButton::builder()
            .label("Voltage")
            .active(true)
            .focus_on_click(false)
            .tooltip_text("Set the Voltage and Current Limit")
            .build();
        let by_power = gtk::ToggleButton::builder()
            .label("Power")
            .group(&by_voltage)
            .focus_on_click(false)
            .tooltip_text("Hold a Constant Power")
            .build();
        let modes = gtk::Box::builder()
            .css_classes(["linked"])
            .valign(gtk::Align::Center)
            .build();
        modes.append(&by_voltage);
        modes.append(&by_power);

        // All in the card: the mode and its switch, what the output is
        // doing, then the values.
        let mode_row = adw::ActionRow::builder()
            .title("Mode")
            .subtitle(VOLTAGE_NOTE)
            .build();
        mode_row.add_suffix(&modes);
        let setpoint = adw::PreferencesGroup::new();
        setpoint.add(&mode_row);
        let status_icon = gtk::Image::builder()
            .icon_name("system-shutdown-symbolic")
            .css_classes(["dim-label"])
            .build();
        let status_spinner = gtk::Spinner::builder().visible(false).build();
        let status = adw::ActionRow::builder()
            .title("Output off")
            .css_classes(["numeric"])
            .build();
        status.add_prefix(&status_icon);
        status.add_prefix(&status_spinner);
        setpoint.add(&status);
        setpoint.add(&p);
        setpoint.add(&v);
        setpoint.add(&i);

        let toggle_content = adw::ButtonContent::builder()
            .icon_name("system-shutdown-symbolic")
            .label("Turn On")
            .build();
        // In the header bar, beside Record.
        let toggle = gtk::Button::builder()
            .child(&toggle_content)
            .valign(gtk::Align::Center)
            .tooltip_text("Turn the Output On")
            .build();

        let c = Rc::new(Self {
            setpoint,
            mode_row,
            status,
            status_icon,
            status_spinner,
            sample: Cell::new(None),
            output: Cell::new(None),
            power: Cell::new(None),
            locked: Cell::new(false),
            toggle,
            toggle_content,
            by_voltage,
            by_power,
            p,
            v,
            i,
            syncing: Cell::new(false),
            timer: RefCell::new(None),
            pending: Cell::new(None),
            on: Cell::new(false),
            mode: Cell::new(Mode::Voltage),
            holding: Cell::new(false),
            starting: Cell::new(false),
            ended: Cell::new(false),
            measured: Cell::new(0.0),
            ctx,
        });

        for row in [&c.p, &c.v, &c.i] {
            let w = Rc::downgrade(&c);
            row.connect_value_notify(move |_| {
                if let Some(c) = w.upgrade() {
                    c.show_reach();
                    if !c.syncing.get() {
                        c.schedule();
                    }
                }
            });
        }
        c.show_reach();

        let w = Rc::downgrade(&c);
        c.by_power.connect_toggled(move |b| {
            if let Some(c) = w.upgrade()
                && !c.syncing.get()
                && b.is_active()
            {
                c.enter_power();
            }
        });
        let w = Rc::downgrade(&c);
        c.by_voltage.connect_toggled(move |b| {
            if let Some(c) = w.upgrade()
                && !c.syncing.get()
                && b.is_active()
            {
                c.enter_voltage();
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

    /// Sends what the rows say now, cancelling any wait. In power mode an
    /// edit only goes out while a hold is running; otherwise Turn On uses
    /// it.
    fn send(&self) {
        if let Some(id) = self.timer.take() {
            id.remove();
        }
        if self.mode.get() == Mode::Power {
            if self.holding.get() {
                self.hold(false);
            }
            return;
        }
        let want = (milli(self.v.value()), milli(self.i.value()));
        self.pending.set(Some((want, Instant::now())));
        self.ctx.act(Action::SetSetpoint {
            vset_mv: want.0,
            iset_ma: want.1,
        });
    }

    fn want(&self) -> PowerTarget {
        PowerTarget {
            power: self.p.value(),
            max_voltage: self.v.value(),
            max_current: self.i.value(),
        }
    }

    fn hold(&self, turn_on: bool) {
        self.starting.set(!self.holding.get());
        self.ctx.command(Command::HoldPower {
            want: self.want(),
            turn_on,
        });
    }

    /// What the dialogs say a hold will do.
    fn describe(&self) -> String {
        let w = self.want();
        format!(
            "The DP100 will hold {}, moving the current limit as needed, at up to {} and {}.",
            watts(w.power),
            volts(milli(w.max_voltage)),
            amps(milli(w.max_current))
        )
    }

    fn enter_power(self: &Rc<Self>) {
        if !self.on.get() {
            self.show_mode(Mode::Power);
            return;
        }
        // The output is on: start from what it delivers now, within the
        // setpoints it has, so nothing jumps.
        self.syncing.set(true);
        self.p
            .set_value((self.measured.get() * 100.0).round().max(1.0) / 100.0);
        self.syncing.set(false);
        let (yes, no) = (Rc::downgrade(self), Rc::downgrade(self));
        self.ctx.ask(
            "Hold Constant Power?",
            &self.describe(),
            "Hold Power",
            adw::ResponseAppearance::Suggested,
            move || {
                if let Some(c) = yes.upgrade() {
                    c.show_mode(Mode::Power);
                    c.hold(false);
                }
            },
            move || {
                if let Some(c) = no.upgrade() {
                    c.show_mode(Mode::Voltage);
                }
            },
        );
    }

    fn enter_voltage(&self) {
        if self.holding.get() || self.starting.get() {
            self.ctx.command(Command::StopPower);
        }
        self.holding.set(false);
        self.starting.set(false);
        self.show_mode(Mode::Voltage);
    }

    /// The most power the limits allow, under the power row, warning when
    /// the target is past it.
    fn show_reach(&self) {
        let reach = self.v.value() * self.i.value();
        if self.p.value() > reach + 1e-9 {
            self.p.set_subtitle(&format!(
                "More than the {} these limits allow",
                watts(reach)
            ));
            self.p.add_css_class("warning");
        } else {
            self.p
                .set_subtitle(&format!("Up to {} within these limits", watts(reach)));
            self.p.remove_css_class("warning");
        }
    }

    /// Puts the rows, titles and toggle in `mode`'s shape.
    fn show_mode(&self, mode: Mode) {
        self.mode.set(mode);
        self.syncing.set(true);
        let power = mode == Mode::Power;
        self.by_power.set_active(power);
        self.by_voltage.set_active(!power);
        self.p.set_visible(power);
        self.v
            .set_title(if power { "Voltage limit" } else { "Voltage" });
        self.mode_row
            .set_subtitle(if power { POWER_NOTE } else { VOLTAGE_NOTE });
        self.syncing.set(false);
        self.show_status();
    }

    /// The status row: what the output is doing, with an icon in the colour
    /// of how it's going, and the figures behind it.
    fn show_status(&self) {
        enum Look {
            Off,
            Good,
            Warn,
            Bad,
            Busy,
            Locked,
        }
        let m = self.sample.get();
        let on = self.on.get();
        let (vset, iset) = self.output.get().unwrap_or((0, 0));
        let power = self.mode.get() == Mode::Power;
        let want = self.want();
        let (look, title, detail): (Look, String, String) = if self.locked.get() {
            (
                Look::Locked,
                "A sequence is in control".into(),
                "It sets the output until it ends".into(),
            )
        } else if let Some(m) = m.filter(|m| m.fault != Fault::Ok) {
            (
                Look::Bad,
                format!("Protection tripped ({})", m.fault.label()),
                "Clear the trip to turn the output on again".into(),
            )
        } else if m.is_some_and(|m| m.mode == DeviceMode::NoInput) {
            (
                Look::Bad,
                "No input power".into(),
                "Connect a USB-C PD charger or a 5–32 V DC supply".into(),
            )
        } else if !on {
            let next = if power {
                format!(
                    "Turn On holds {} at up to {} and {}",
                    watts(want.power),
                    volts(milli(want.max_voltage)),
                    amps(milli(want.max_current))
                )
            } else {
                format!(
                    "Turn On supplies {} with a {} limit",
                    volts(vset),
                    amps(iset)
                )
            };
            (Look::Off, "Output off".into(), next)
        } else if power {
            // What the worker is holding, and its average over the last
            // update, which a single reading of a noisy load isn't.
            let held = self.power.get();
            let want = held.map_or(want, |p| p.want);
            let now = held.map_or_else(
                || m.map_or(0.0, |m| f64::from(m.power_mw()) / 1000.0),
                |p| p.power,
            );
            match held.map(|p| p.state) {
                Some(HoldState::Holding) => (
                    Look::Good,
                    format!("Holding {}", watts(want.power)),
                    format!("{} now, by moving the current limit", watts(now)),
                ),
                Some(HoldState::LimitedByVoltage) => (
                    Look::Warn,
                    "Limited by the voltage limit".into(),
                    format!(
                        "{} at {}: the load takes no more there",
                        watts(now),
                        volts(milli(want.max_voltage))
                    ),
                ),
                Some(HoldState::LimitedByCurrent) => (
                    Look::Warn,
                    "Limited by the current limit".into(),
                    format!(
                        "{} at {}: raise it to reach {}",
                        watts(now),
                        amps(milli(want.max_current)),
                        watts(want.power)
                    ),
                ),
                _ => (
                    Look::Busy,
                    format!("Settling on {}", watts(want.power)),
                    format!("{} now", watts(now)),
                ),
            }
        } else if m.is_some_and(|m| m.mode == DeviceMode::ConstantCurrent) {
            (
                Look::Warn,
                "Limiting current".into(),
                format!(
                    "At the {} limit; the voltage has fallen to {}",
                    amps(iset),
                    volts(m.map_or(0, |m| m.vout_mv))
                ),
            )
        } else {
            (
                Look::Good,
                "Regulating voltage".into(),
                format!(
                    "Holding {} · {} of the {} limit",
                    volts(vset),
                    amps(m.map_or(0, |m| m.iout_ma)),
                    amps(iset)
                ),
            )
        };
        let (icon, class) = match look {
            Look::Off => ("system-shutdown-symbolic", "dim-label"),
            Look::Good => ("object-select-symbolic", "success"),
            Look::Warn => ("dialog-warning-symbolic", "warning"),
            Look::Bad => ("dialog-error-symbolic", "error"),
            Look::Busy => ("content-loading-symbolic", "accent"),
            Look::Locked => ("changes-prevent-symbolic", "dim-label"),
        };
        let busy = matches!(look, Look::Busy);
        self.status_spinner.set_visible(busy);
        self.status_spinner.set_spinning(busy);
        self.status_icon.set_visible(!busy);
        self.status_icon.set_icon_name(Some(icon));
        self.status_icon.set_css_classes(&[class]);
        if self.status.title() != title {
            self.status.set_title(&title);
        }
        if self.status.subtitle().as_deref() != Some(detail.as_str()) {
            self.status.set_subtitle(&detail);
        }
    }

    /// While a sequence runs it owns the setpoints.
    pub fn set_locked(&self, locked: bool) {
        for w in [
            self.p.upcast_ref::<gtk::Widget>(),
            self.v.upcast_ref(),
            self.i.upcast_ref(),
            self.by_voltage.upcast_ref(),
            self.by_power.upcast_ref(),
        ] {
            w.set_sensitive(!locked);
        }
        self.locked.set(locked);
        self.show_status();
    }

    pub fn is_holding(&self) -> bool {
        self.holding.get()
    }

    fn toggle_output(self: &Rc<Self>) {
        if self.on.get() {
            self.ctx.act(Action::SetOutput(false));
            return;
        }
        // An adjustment still waiting to go out goes first, so the output
        // comes on at what the rows show.
        if self.timer.borrow().is_some() {
            self.send();
        }
        if self.mode.get() == Mode::Power {
            let w = Rc::downgrade(self);
            self.ctx.confirm(
                "Turn on the output?",
                &self.describe(),
                "Turn On",
                adw::ResponseAppearance::Suggested,
                move || {
                    if let Some(c) = w.upgrade() {
                        c.hold(true);
                    }
                },
            );
            return;
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
        self.output.set(Some((o.vset_mv, o.iset_ma)));
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
        // In power mode the rows hold what was asked for; the worker moves
        // the device's setpoints.
        let follow = if self.mode.get() == Mode::Power || self.timer.borrow().is_some() {
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
        self.measured.set(f64::from(m.power_mw()) / 1000.0);
        if on != self.on.get() {
            self.on.set(on);
            if on {
                self.toggle_content.set_label("Turn Off");
                self.toggle.add_css_class("destructive-action");
                self.toggle
                    .set_tooltip_text(Some("Turn the Output Off (F9)"));
            } else {
                self.toggle_content.set_label("Turn On");
                self.toggle.remove_css_class("destructive-action");
                self.toggle.set_tooltip_text(Some("Turn the Output On"));
            }
        }
        // A hold that ended with the output still on (a change made on the
        // panel, say) leaves the device's own setpoints in charge, so the
        // rows go back to following them. One that ended with the output
        // off stays in power mode, ready to turn on again.
        if self.ended.take() && on {
            self.show_mode(Mode::Voltage);
        }
        // Off always works. On needs input power and no latched trip.
        self.toggle
            .set_sensitive(on || (m.mode != DeviceMode::NoInput && m.fault == Fault::Ok));
        self.v.set_subtitle(&format!(
            "Up to {} available from this input",
            volts(m.vo_max_mv)
        ));
        self.sample.set(Some(*m));
        self.show_status();
    }

    pub fn show_power(&self, st: &PowerStatus) {
        self.holding.set(true);
        self.starting.set(false);
        if self.mode.get() != Mode::Power {
            self.show_mode(Mode::Power);
        }
        self.power.set(Some(*st));
        self.show_status();
    }

    /// The worker stopped holding power.
    pub fn power_ended(&self) {
        self.holding.set(false);
        self.starting.set(false);
        self.power.set(None);
        if self.mode.get() == Mode::Power {
            self.ended.set(true);
        }
    }

    /// An action failed. If it was a hold starting, nothing is holding: a
    /// switch to power mode with the output on goes back to voltage.
    pub fn refused(&self) {
        if self.starting.take() && !self.holding.get() && self.on.get() {
            self.show_mode(Mode::Voltage);
        }
    }
}

/// Watts, as the rows and notes show them.
pub fn watts(w: f64) -> String {
    format!("{w:.2}\u{202f}W")
}
