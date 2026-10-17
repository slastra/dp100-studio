mod controls;
mod ctx;
mod device;
mod graph;
mod history;
mod log;
mod presets;
mod readouts;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::sim::{SimDevice, State};
use dp100::{Fault, Mode};
use gtk::{gdk, gio, glib};

use crate::worker::{Action, Command, Event, Problem, Source, Worker};
use controls::Controls;
use ctx::Ctx;
use device::Device;
use log::Log;
use presets::Presets;
use readouts::Readouts;

const USBD_HINT: &str = "Connect it over USB and put it in USBD mode: double-tap ◀ on the panel.";

/// A simulated unit that looks like a bench in use: output on at 5 V into a
/// 100 Ω load, so it sits in CV at 50 mA.
fn demo_sim() -> Source {
    let mut s = State::default();
    s.load_ohms = Some(100.0);
    s.noise = true;
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
    ctx: Rc<Ctx>,
    header: adw::HeaderBar,
    title: adw::WindowTitle,
    switcher: adw::ViewSwitcher,
    output_chip: gtk::Label,
    recording_chip: gtk::Label,
    banner: adw::Banner,
    stack: gtk::Stack,
    status: adw::StatusPage,
    reconnect: gtk::Button,
    readouts: Readouts,
    presets: Rc<Presets>,
    device: Rc<Device>,
    log: Rc<Log>,
    simulated: bool,
}

pub fn activate(app: &adw::Application, simulate: bool) {
    if let Some(w) = app.active_window() {
        w.present();
        return;
    }
    load_css();
    install_app_actions(app);

    let source = if simulate {
        demo_sim()
    } else {
        Source::Hardware
    };
    let worker = Worker::spawn(source);
    let w = Rc::new(build(app, worker.commands.clone(), simulate));
    install_window_actions(app, &w);

    let events = worker.events.clone();
    // The worker lives as long as the window; dropping it stops the thread.
    let worker = RefCell::new(Some(worker));
    w.ctx.window.connect_close_request(move |_| {
        worker.borrow_mut().take();
        glib::Propagation::Proceed
    });

    let ui = w.clone();
    glib::spawn_future_local(async move {
        while let Ok(ev) = events.recv().await {
            ui.apply(ev);
        }
    });

    w.ctx.window.present();
}

fn install_app_actions(app: &adw::Application) {
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

fn install_window_actions(app: &adw::Application, w: &Rc<Window>) {
    // Always available, never asks: the panic button.
    let ctx = w.ctx.clone();
    let off = gio::ActionEntry::builder("output-off")
        .activate(move |_: &adw::ApplicationWindow, _, _| ctx.act(Action::SetOutput(false)))
        .build();
    let ctx = w.ctx.clone();
    let release = gio::ActionEntry::builder("release")
        .activate(move |_: &adw::ApplicationWindow, _, _| ctx.command(Command::Release))
        .build();
    w.ctx.window.add_action_entries([off, release]);
    app.set_accels_for_action("win.output-off", &["F9"]);

    let ctx = w.ctx.clone();
    w.reconnect
        .connect_clicked(move |_| ctx.command(Command::Reclaim));
    let ctx = w.ctx.clone();
    w.banner
        .connect_button_clicked(move |_| ctx.act(Action::ClearFault));
}

fn page<W: IsA<gtk::Widget>>(child: &W) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .child(child)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build()
}

fn build(
    app: &adw::Application,
    cmds: crossbeam_channel::Sender<Command>,
    simulated: bool,
) -> Window {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(if simulated {
            "DP100 Studio (simulated)"
        } else {
            "DP100 Studio"
        })
        .default_width(640)
        .default_height(900)
        .build();
    let toasts = adw::ToastOverlay::new();
    let ctx = Rc::new(Ctx::new(cmds, window.clone(), toasts.clone()));

    let title = adw::WindowTitle::new("DP100 Studio", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let output_chip = gtk::Label::builder()
        .label("OUTPUT ON")
        .css_classes(["output-chip"])
        .tooltip_text("Press F9 to turn the output off")
        .visible(false)
        .build();
    header.pack_start(&output_chip);
    let recording_chip = gtk::Label::builder()
        .label("REC")
        .css_classes(["recording-chip"])
        .tooltip_text("Recording to CSV. Stop it on the Log page.")
        .visible(false)
        .build();
    header.pack_start(&recording_chip);

    let menu = gio::Menu::new();
    let device_section = gio::Menu::new();
    device_section.append(Some("Turn Output Off"), Some("win.output-off"));
    device_section.append(Some("Release Front Panel"), Some("win.release"));
    menu.append_section(None, &device_section);
    let app_section = gio::Menu::new();
    app_section.append(Some("About DP100 Studio"), Some("app.about"));
    menu.append_section(None, &app_section);
    header.pack_end(
        &gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main menu")
            .build(),
    );

    let banner = adw::Banner::new("");

    let reconnect = gtk::Button::builder()
        .label("Reconnect")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    let status = adw::StatusPage::builder()
        .icon_name("uninterruptible-power-supply-symbolic")
        .title("Looking for a DP100")
        .description(USBD_HINT)
        .child(&reconnect)
        .build();

    let controls = Controls::new(ctx.clone());
    let readouts = Readouts::new(controls);
    let presets = Presets::new(ctx.clone());
    let device = Device::new(ctx.clone(), simulated);
    let log = Log::new(ctx.clone());

    let views = adw::ViewStack::new();
    views.add_titled_with_icon(
        &page(&readouts.root),
        Some("monitor"),
        "Monitor",
        "utilities-system-monitor-symbolic",
    );
    views.add_titled_with_icon(
        &presets.root,
        Some("presets"),
        "Presets",
        "view-list-symbolic",
    );
    views.add_titled_with_icon(&log.root, Some("log"), "Log", "media-record-symbolic");
    views.add_titled_with_icon(
        &device.root,
        Some("device"),
        "Device",
        "preferences-system-symbolic",
    );
    // A development hook for screenshots: start on another page.
    if let Ok(name) = std::env::var("DP100_STUDIO_PAGE") {
        views.set_visible_child_name(&name);
    }
    let switcher = adw::ViewSwitcher::builder()
        .stack(&views)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&status, Some("status"));
    stack.add_named(&views, Some("device"));
    toasts.set_child(Some(&stack));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);
    view.set_content(Some(&toasts));
    window.set_content(Some(&view));

    Window {
        ctx,
        header,
        title,
        switcher,
        output_chip,
        recording_chip,
        banner,
        stack,
        status,
        reconnect,
        readouts,
        presets,
        device,
        log,
        simulated,
    }
}

impl Window {
    /// Shows the status page in place of the device pages.
    fn status(&self, title: &str, description: &str) {
        self.status.set_title(title);
        self.status.set_description(Some(description));
        self.reconnect.set_visible(false);
        self.output_chip.set_visible(false);
        self.set_banner(None, false);
        self.header.set_title_widget(Some(&self.title));
        self.title
            .set_subtitle(if self.simulated { "Simulated" } else { "" });
        self.stack.set_visible_child_name("status");
    }

    fn set_banner(&self, text: Option<&str>, clear_button: bool) {
        match text {
            Some(t) => {
                self.banner.set_title(t);
                self.banner
                    .set_button_label(clear_button.then_some("Clear Trip"));
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }

    fn apply(&self, ev: Event) {
        match ev {
            Event::Searching => self.status("Looking for a DP100", USBD_HINT),
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
                self.status("DP100 found, but not usable", &text);
            }
            Event::Connected(s) => {
                self.set_banner(None, false);
                self.header.set_title_widget(Some(&self.switcher));
                self.ctx.live.borrow_mut().output = Some(s.output);
                self.ctx.live.borrow_mut().system = Some(s.system);
                self.readouts.show_output(&s.output);
                self.presets.show_active(s.output.index);
                self.device.show_info(&s.info);
                self.device.show_output(&s.output);
                self.device.show_system(&s.system);
                self.stack.set_visible_child_name("device");
            }
            Event::Sample(s) => {
                let m = s.m;
                self.ctx.live.borrow_mut().sample = Some(m);
                self.readouts.show_sample(s.at, &m);
                self.output_chip.set_visible(m.output_on().unwrap_or(false));
                if m.fault != Fault::Ok {
                    self.set_banner(
                        Some(&format!(
                            "Protection tripped ({}). The output is held off until the trip is cleared.",
                            m.fault.label()
                        )),
                        true,
                    );
                } else if m.mode == Mode::NoInput {
                    self.set_banner(
                        Some("No input power. Connect a USB-C PD charger or a 5–32 V DC supply."),
                        false,
                    );
                } else {
                    self.set_banner(None, false);
                }
            }
            Event::Output(o) => {
                self.ctx.live.borrow_mut().output = Some(o);
                self.readouts.show_output(&o);
                self.presets.show_active(o.index);
                self.device.show_output(&o);
            }
            Event::Presets(p) => {
                self.presets.show_presets(&p);
                self.ctx.live.borrow_mut().presets = p;
            }
            Event::System(s) => {
                self.ctx.live.borrow_mut().system = Some(s);
                self.device.show_system(&s);
            }
            Event::Energy {
                mwh,
                mah,
                covered,
                gaps,
            } => {
                self.readouts.show_energy(mwh, mah);
                self.log.show_energy(mwh, mah, covered, gaps);
            }
            Event::Recording(r) => {
                self.recording_chip.set_visible(true);
                self.log.show_recording(&r);
            }
            Event::RecordingStopped(r) => {
                self.recording_chip.set_visible(false);
                self.log.show_stopped(&r);
            }
            Event::Done(Ok(Some(note))) => self.ctx.toast(&note),
            Event::Done(Ok(None)) => {}
            Event::Done(Err(e)) => self.ctx.error(&e),
            Event::Released => {
                self.status(
                    "Front panel released",
                    "The DP100's buttons work again, and other programs can open it. \
                     DP100 Studio is not reading the supply until you reconnect.",
                );
                self.reconnect.set_visible(true);
            }
            Event::Disconnected(why) => self.status(
                "DP100 disconnected",
                &format!("{why}. Reconnect it and put it in USBD mode: double-tap ◀ on the panel."),
            ),
        }
    }
}
