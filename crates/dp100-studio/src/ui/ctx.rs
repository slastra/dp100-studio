//! What every page shares: the way to the worker, the latest device state,
//! and the window for dialogs and toasts.

use std::cell::RefCell;

use adw::prelude::*;
use crossbeam_channel::Sender;
use dp100::{BasicInfo, Setpoints, SystemSettings};

use crate::worker::{Action, Command};

/// The most recent state the worker reported.
#[derive(Default)]
pub struct Live {
    pub sample: Option<BasicInfo>,
    pub output: Option<Setpoints>,
    pub presets: Vec<Setpoints>,
    pub system: Option<SystemSettings>,
}

pub struct Ctx {
    cmds: Sender<Command>,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    pub live: RefCell<Live>,
}

impl Ctx {
    pub fn new(
        cmds: Sender<Command>,
        window: adw::ApplicationWindow,
        toasts: adw::ToastOverlay,
    ) -> Self {
        Self {
            cmds,
            window,
            toasts,
            live: RefCell::new(Live::default()),
        }
    }

    pub fn act(&self, a: Action) {
        let _ = self.cmds.send(Command::Do(a));
    }

    pub fn command(&self, c: Command) {
        let _ = self.cmds.send(c);
    }

    /// Toast titles are markup unless told otherwise, and these carry
    /// sequence names and step text with "<" and "&" in them.
    ///
    /// A single trailing period is dropped: GNOME's writing style ends no
    /// toast with one, while many messages here come from the worker as
    /// full sentences.
    fn plain_toast(text: &str) -> adw::Toast {
        let t = adw::Toast::builder().use_markup(false).build();
        t.set_title(text.strip_suffix('.').unwrap_or(text));
        t
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(Self::plain_toast(text));
    }

    pub fn error(&self, text: &str) {
        let t = Self::plain_toast(text);
        t.set_timeout(8);
        t.set_priority(adw::ToastPriority::High);
        self.toasts.add_toast(t);
    }

    /// Whether the output is on, from the measurements.
    pub fn output_on(&self) -> bool {
        self.live
            .borrow()
            .sample
            .and_then(|m| m.output_on())
            .unwrap_or(false)
    }

    /// Asks before doing something. `accept` runs only if the user picks
    /// `accept_label`; dismissing the dialog is a no.
    pub fn confirm(
        &self,
        heading: &str,
        body: &str,
        accept_label: &str,
        appearance: adw::ResponseAppearance,
        accept: impl Fn() + 'static,
    ) {
        self.ask(heading, body, accept_label, appearance, accept, || {});
    }

    /// [`Ctx::confirm`] with something to do on Cancel too.
    pub fn ask(
        &self,
        heading: &str,
        body: &str,
        accept_label: &str,
        appearance: adw::ResponseAppearance,
        accept: impl Fn() + 'static,
        cancel: impl Fn() + 'static,
    ) {
        let d = adw::AlertDialog::new(Some(heading), Some(body));
        d.add_responses(&[("cancel", "Cancel"), ("accept", accept_label)]);
        d.set_response_appearance("accept", appearance);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        d.connect_response(None, move |_, r| {
            if r == "accept" {
                accept();
            } else {
                cancel();
            }
        });
        d.present(Some(&self.window));
    }
}

/// Values and units are joined by a narrow no-break space (U+202F), as
/// GNOME's typography guidelines ask, so "12.000 V" never breaks apart.
pub fn volts(mv: u16) -> String {
    format!("{:.3}\u{202f}V", f64::from(mv) / 1000.0)
}

pub fn amps(ma: u16) -> String {
    format!("{:.3}\u{202f}A", f64::from(ma) / 1000.0)
}

/// Volts or amps from a spin button, as the device's milli-units.
pub fn milli(x: f64) -> u16 {
    (x * 1000.0).round().clamp(0.0, f64::from(u16::MAX)) as u16
}
