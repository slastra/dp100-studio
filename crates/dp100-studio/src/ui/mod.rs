mod readouts;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::sim::{SimDevice, State};
use dp100::{Fault, Mode};
use gtk::{gdk, gio, glib};

use crate::worker::{Event, Problem, Source, Worker};
use readouts::Readouts;

const USBD_HINT: &str = "Connect it over USB and put it in USBD mode: double-tap ◀ on the panel.";

/// A simulated unit that looks like a bench in use: output on at 5 V into a
/// 100 Ω load, so it sits in CV at 50 mA.
fn demo_sim() -> Source {
    let mut s = State::default();
    s.load_ohms = Some(100.0);
    s.live.vset_mv = 5_000;
    s.live.iset_ma = 100;
    s.live.on = true;
    s.output_on = true;
    let (_, handle) = SimDevice::with_state(s);
    Source::Simulated(handle)
}

fn load_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("a display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

struct Window {
    window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    output_chip: gtk::Label,
    banner: adw::Banner,
    stack: gtk::Stack,
    status: adw::StatusPage,
    readouts: Readouts,
    simulated: bool,
}

pub fn activate(app: &adw::Application, simulate: bool) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    load_css();
    install_actions(app);

    let source = if simulate {
        demo_sim()
    } else {
        Source::Hardware
    };
    let worker = Worker::spawn(source);
    let w = Rc::new(build(app, simulate));

    let events = worker.events.clone();
    // The worker lives as long as the window; dropping it stops the thread.
    let worker = RefCell::new(Some(worker));
    w.window.connect_close_request(move |_| {
        worker.borrow_mut().take();
        glib::Propagation::Proceed
    });

    let ui = w.clone();
    glib::spawn_future_local(async move {
        while let Ok(ev) = events.recv().await {
            ui.apply(ev);
        }
    });

    w.window.present();
}

fn install_actions(app: &adw::Application) {
    let about = gio::ActionEntry::builder("about")
        .activate(|app: &adw::Application, _, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("DP100 Studio")
                .application_icon(crate::APP_ID)
                .developer_name("Shaun Lastra")
                .version(env!("CARGO_PKG_VERSION"))
                .license_type(gtk::License::Gpl30)
                .website("https://github.com/slastra/dp100-studio")
                .issue_url("https://github.com/slastra/dp100-studio/issues")
                .comments(
                    "Control and monitor the ALIENTEK DP100 bench power supply.\n\n\
                     Not affiliated with or endorsed by ALIENTEK.",
                )
                .build();
            dialog.add_credit_section(
                Some("Protocol research"),
                &[
                    "jjcarrier/vicon https://github.com/jjcarrier/vicon",
                    "weigu1/dp100_manipulator https://github.com/weigu1/dp100_manipulator",
                ],
            );
            dialog.present(app.active_window().as_ref());
        })
        .build();
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build();
    app.add_action_entries([about, quit]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

fn build(app: &adw::Application, simulated: bool) -> Window {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("DP100 Studio")
        .default_width(560)
        .default_height(720)
        .build();

    let title = adw::WindowTitle::new("DP100 Studio", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let output_chip = gtk::Label::builder()
        .label("OUTPUT ON")
        .css_classes(["output-chip"])
        .visible(false)
        .build();
    header.pack_start(&output_chip);

    let menu = gio::Menu::new();
    menu.append(Some("About DP100 Studio"), Some("app.about"));
    header.pack_end(
        &gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main menu")
            .build(),
    );

    let banner = adw::Banner::new("");

    let status = adw::StatusPage::builder()
        .icon_name("uninterruptible-power-supply-symbolic")
        .title("Looking for a DP100")
        .description(USBD_HINT)
        .build();

    let readouts = Readouts::new();
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .child(&readouts.root)
        .build();
    let device = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&status, Some("status"));
    stack.add_named(&device, Some("device"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);
    view.set_content(Some(&stack));
    window.set_content(Some(&view));

    Window {
        window,
        title,
        output_chip,
        banner,
        stack,
        status,
        readouts,
        simulated,
    }
}

impl Window {
    fn subtitle(&self, s: &str) {
        if self.simulated {
            self.title.set_subtitle(&format!("{s} (simulated)"));
        } else {
            self.title.set_subtitle(s);
        }
    }

    fn set_banner(&self, text: Option<&str>) {
        match text {
            Some(t) => {
                self.banner.set_title(t);
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }

    fn apply(&self, ev: Event) {
        match ev {
            Event::Searching => {
                self.set_banner(None);
                self.subtitle("Not connected");
                self.status.set_title("Looking for a DP100");
                self.status.set_description(Some(USBD_HINT));
                self.output_chip.set_visible(false);
                self.stack.set_visible_child_name("status");
            }
            Event::Problem(p) => {
                let text = match &p {
                    Problem::PermissionDenied(path) => format!(
                        "No permission to open {}. Install the udev rule (see README) and replug.",
                        path.display()
                    ),
                    Problem::Busy(path) => format!(
                        "{} is in use by another program. Close it to connect.",
                        path.display()
                    ),
                    Problem::Other(e) => format!("Can't open the DP100: {e}"),
                };
                self.status.set_title("DP100 found, but not usable");
                self.status.set_description(Some(&text));
                self.stack.set_visible_child_name("status");
            }
            Event::Connected(s) => {
                self.set_banner(None);
                self.subtitle(&format!("{} · firmware {}", s.info.model, s.info.firmware));
                self.readouts.show_output(&s.output);
                self.readouts.show_system(&s.system);
                self.stack.set_visible_child_name("device");
            }
            Event::Sample(s) => {
                let m = s.m;
                self.readouts.show_sample(&m);
                self.output_chip.set_visible(m.output_on().unwrap_or(false));
                let warn = if m.fault != Fault::Ok {
                    Some(format!(
                        "Protection tripped ({}). The output is held off until the trip is cleared.",
                        m.fault.label()
                    ))
                } else if m.mode == Mode::NoInput {
                    Some("No input power. Connect a USB-C PD charger or a 5–32 V DC supply.".into())
                } else {
                    None
                };
                self.set_banner(warn.as_deref());
            }
            Event::Output(o) => self.readouts.show_output(&o),
            Event::Disconnected(why) => {
                self.subtitle("Disconnected");
                self.status.set_title("DP100 disconnected");
                self.status.set_description(Some(&format!(
                    "{why}. Reconnect it and put it in USBD mode: double-tap ◀ on the panel."
                )));
                self.output_chip.set_visible(false);
                self.stack.set_visible_child_name("status");
            }
        }
    }
}
