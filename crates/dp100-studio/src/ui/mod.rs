mod controls;
mod ctx;
mod device;
mod graph;
mod history;
mod log;
mod page;
mod panel;
mod presets;
mod readouts;
mod sequences;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use dp100::sim::{Board, SimDevice, State};
use dp100::{Fault, Mode};
use gtk::{gdk, gio, glib};

use crate::worker::{Action, Command, Event, Problem, Source, Worker};
use controls::Controls;
use ctx::Ctx;
pub use ctx::{amps, volts};
use device::Device;
use log::Log;
use presets::Presets;
use readouts::Readouts;
use sequences::Sequences;

const USBD_HINT: &str = "Connect it over USB and put it in USBD mode: double-tap ◀ on its panel";

/// The pages, for Alt+1 to Alt+5: the main window, then the dialogs.
const PAGES: [&str; 5] = ["monitor", "presets", "sequences", "log", "device"];

const SHORTCUTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="help_overlay">
    <property name="modal">True</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">main</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Output</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Turn Output Off</property>
                <property name="accelerator">F9</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">Windows</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Presets, Sequences, Recording, Device</property>
                <property name="accelerator">&lt;Alt&gt;2...&lt;Alt&gt;5</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Device Settings</property>
                <property name="accelerator">&lt;Control&gt;comma</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Back to the Graph</property>
                <property name="accelerator">&lt;Alt&gt;1</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title">General</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Main Menu</property>
                <property name="accelerator">F10</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Keyboard Shortcuts</property>
                <property name="accelerator">&lt;Control&gt;question</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Close Window</property>
                <property name="accelerator">&lt;Control&gt;w</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title">Quit</property>
                <property name="accelerator">&lt;Control&gt;q</property>
              </object>
            </child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>"#;

/// Where the window's size is remembered between runs.
fn window_state_file() -> std::path::PathBuf {
    // XDG_STATE_HOME, as glib::user_state_dir() would give (it needs a
    // newer glib feature than the floor here).
    std::env::var_os("XDG_STATE_HOME")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| glib::home_dir().join(".local").join("state"))
        .join("dp100-studio")
        .join("window")
}

/// The window as it was left: its size, whether it was maximized, and how
/// wide the panel was.
struct WindowState {
    width: i32,
    height: i32,
    maximized: bool,
    panel: Option<i32>,
}

fn load_window_state() -> Option<WindowState> {
    let text = std::fs::read_to_string(window_state_file()).ok()?;
    let mut it = text.split_whitespace();
    let width = it.next()?.parse().ok()?;
    let height = it.next()?.parse().ok()?;
    let maximized = it.next().is_some_and(|m| m == "1");
    let panel = it.next().and_then(|p| p.parse().ok());
    (width >= 360 && height >= 294).then_some(WindowState {
        width,
        height,
        maximized,
        panel,
    })
}

fn save_window_state(win: &adw::ApplicationWindow, panel: i32) {
    let (w, h) = win.default_size();
    let path = window_state_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        format!("{w} {h} {} {panel}\n", u8::from(win.is_maximized())),
    );
}

/// A simulated unit that looks like a bench in use: output on at 5 V into a
/// microcontroller board, so it sits in CV with the board's radio bursts.
fn demo_sim() -> Source {
    let mut s = State::default();
    // A development hook for screenshots of loads other than the board.
    match std::env::var("DP100_STUDIO_SIM_OHMS")
        .ok()
        .and_then(|r| r.parse().ok())
    {
        Some(ohms) => s.load_ohms = Some(ohms),
        None => s.board = Some(Board::new()),
    }
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
    title: adw::WindowTitle,
    /// The panel of readings and setpoints, and the graph, with a handle
    /// between them to size the panel.
    paned: gtk::Paned,
    /// The panel's width while side by side, to restore it and remember it.
    panel_width: std::cell::Cell<i32>,
    /// The device's pages, opened from the main menu.
    presets_dialog: adw::Dialog,
    sequences_dialog: adw::Dialog,
    log_dialog: adw::Dialog,
    device_dialog: adw::PreferencesDialog,
    /// A page to open once a DP100 connects (a development hook).
    open_on_connect: RefCell<Option<String>>,
    /// The device pages are showing (not the status page).
    connected: std::cell::Cell<bool>,
    banner: adw::Banner,
    stack: gtk::Stack,
    status: adw::StatusPage,
    reconnect: gtk::Button,
    setup_help: gtk::Button,
    readouts: Readouts,
    presets: Rc<Presets>,
    device: Rc<Device>,
    log: Rc<Log>,
    sequences: Rc<Sequences>,
    simulated: bool,
    /// Suspend and logout held off while recording or running a sequence.
    inhibit: std::cell::Cell<Option<u32>>,
    recording: std::cell::Cell<bool>,
    last_fault: std::cell::Cell<Fault>,
    /// For the About dialog's troubleshooting section.
    debug_info: RefCell<String>,
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
    install_breakpoint(&w);

    let events = worker.events.clone();
    // The worker lives as long as the window; dropping it stops the thread.
    let worker = RefCell::new(Some(worker));
    let confirmed = std::cell::Cell::new(false);
    let ui = w.clone();
    w.ctx.window.connect_close_request(move |win| {
        let panel = ui.panel_width.get();
        // Closing mid-sequence stops it, applying its on_abort, so ask.
        if ui.sequences.is_running() && !confirmed.get() {
            let win = win.clone();
            let ui2 = ui.clone();
            ui.ctx.confirm(
                "Stop the Sequence and Quit?",
                "A sequence is running. Quitting stops it, and applies its \"if it stops early\" setting",
                "Stop and Quit",
                adw::ResponseAppearance::Destructive,
                move || {
                    save_window_state(&win, panel);
                    let _ = &ui2;
                    win.destroy();
                },
            );
            return glib::Propagation::Stop;
        }
        // Quitting ends a power hold; the output stays at its last values,
        // no longer adjusted, so say so first.
        if ui.readouts.is_holding() && !confirmed.get() {
            let win = win.clone();
            ui.ctx.confirm(
                "Stop Holding Power and Quit?",
                "Power mode is holding the output. Quitting stops adjusting it, \
                 and the output stays on at its last settings",
                "Quit",
                adw::ResponseAppearance::Destructive,
                move || {
                    save_window_state(&win, panel);
                    win.destroy();
                },
            );
            return glib::Propagation::Stop;
        }
        save_window_state(win, panel);
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
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| app.quit())
        .build();
    app.add_action_entries([quit]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

impl Window {
    fn about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name("DP100 Studio")
            .application_icon(crate::APP_ID)
            .developer_name("Shaun Lastra")
            .version(env!("CARGO_PKG_VERSION"))
            .license_type(gtk::License::Gpl30)
            .website("https://github.com/slastra/dp100-studio")
            .issue_url("https://github.com/slastra/dp100-studio/issues")
            .comments(
                "Control and monitor the ALIENTEK DP100 bench power supply\n\n\
                 Not affiliated with or endorsed by ALIENTEK",
            )
            .debug_info(self.debug_text())
            .debug_info_filename("dp100-studio-debug.txt")
            .build();
        dialog.add_credit_section(
            Some("Protocol Research"),
            &[
                "jjcarrier/vicon https://github.com/jjcarrier/vicon",
                "weigu1/dp100_manipulator https://github.com/weigu1/dp100_manipulator",
            ],
        );
        dialog.present(Some(&self.ctx.window));
    }

    /// Plain text for bug reports: versions of everything involved.
    fn debug_text(&self) -> String {
        let device = self.debug_info.borrow();
        format!(
            "DP100 Studio {}\nGTK {}.{}.{}\nlibadwaita {}.{}.{}\n{}{}\n",
            env!("CARGO_PKG_VERSION"),
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version(),
            if device.is_empty() {
                "No DP100 connected"
            } else {
                device.as_str()
            },
            if self.simulated { " (simulated)" } else { "" },
        )
    }

    /// Holds off suspend and logout while something is running that a sleep
    /// would ruin, and lets go when it stops.
    fn update_inhibit(&self) {
        let Some(app) = self.ctx.window.application() else {
            return;
        };
        let reason = if self.sequences.is_running() {
            Some("Running a sequence on the DP100")
        } else if self.readouts.is_holding() {
            Some("Holding a constant power on the DP100")
        } else if self.recording.get() {
            Some("Recording the DP100's measurements")
        } else {
            None
        };
        if let Some(cookie) = self.inhibit.take() {
            app.uninhibit(cookie);
        }
        if let Some(reason) = reason {
            let cookie = app.inhibit(
                Some(&self.ctx.window),
                gtk::ApplicationInhibitFlags::SUSPEND | gtk::ApplicationInhibitFlags::LOGOUT,
                Some(reason),
            );
            if cookie != 0 {
                self.inhibit.set(Some(cookie));
            }
        }
    }

    /// A desktop notification, only when the user is in another window;
    /// otherwise the toast or banner in this one says the same.
    fn notify(&self, id: &str, title: &str, body: &str, page: &str) {
        if self.ctx.window.is_active() {
            return;
        }
        let Some(app) = self.ctx.window.application() else {
            return;
        };
        let n = gio::Notification::new(title);
        n.set_body(Some(body));
        n.set_default_action_and_target_value("app.show-page", Some(&page.to_variant()));
        app.send_notification(Some(id), &n);
    }
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
    let ui = w.clone();
    let about = gio::ActionEntry::builder("about")
        .activate(move |_: &adw::ApplicationWindow, _, _| ui.about())
        .build();
    let close = gio::ActionEntry::builder("close")
        .activate(|win: &adw::ApplicationWindow, _, _| win.close())
        .build();
    let ui = w.clone();
    let page = gio::ActionEntry::builder("page")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(move |_: &adw::ApplicationWindow, _, v| {
            if let Some(name) = v.and_then(|v| v.str()) {
                ui.show_page(name);
            }
        })
        .build();
    w.ctx
        .window
        .add_action_entries([off, release, about, close, page]);
    app.set_accels_for_action("win.page::device", &["<Control>comma", "<Alt>5"]);
    app.set_accels_for_action("win.output-off", &["F9"]);
    app.set_accels_for_action("win.close", &["<Control>w"]);
    for (k, name) in PAGES.iter().enumerate().take(4) {
        app.set_accels_for_action(&format!("win.page::{name}"), &[&format!("<Alt>{}", k + 1)]);
    }

    // A notification's default action raises the window on the page it
    // is about.
    let ui = w.clone();
    let show = gio::ActionEntry::builder("show-page")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(move |_: &adw::Application, _, v| {
            ui.ctx.window.present();
            if let Some(name) = v.and_then(|v| v.str()) {
                ui.show_page(name);
            }
        })
        .build();
    app.add_action_entries([show]);

    let shortcuts = gtk::Builder::from_string(SHORTCUTS)
        .object::<gtk::ShortcutsWindow>("help_overlay")
        .expect("shortcuts window in the builder string");
    w.ctx.window.set_help_overlay(Some(&shortcuts));

    let ctx = w.ctx.clone();
    w.reconnect
        .connect_clicked(move |_| ctx.command(Command::Reclaim));
    let ctx = w.ctx.clone();
    w.banner
        .connect_button_clicked(move |_| ctx.act(Action::ClearFault));
}

fn install_breakpoint(w: &Rc<Window>) {
    // A narrow window stacks the panel over the graph.
    let bp = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 760sp").expect("valid condition"),
    );
    bp.add_setter(
        &w.paned,
        "orientation",
        Some(&gtk::Orientation::Vertical.to_value()),
    );
    w.readouts.narrow_layout(&bp);
    let ui = w.clone();
    bp.connect_apply(move |_| {
        let h = ui.ctx.window.height().max(ui.ctx.window.default_height());
        ui.paned.set_position(h * 11 / 20);
    });
    let ui = w.clone();
    bp.connect_unapply(move |_| ui.paned.set_position(ui.panel_width.get()));
    w.ctx.window.add_breakpoint(bp);

    // The panel's width, while side by side, is remembered.
    let ui = Rc::downgrade(w);
    w.paned.connect_position_notify(move |p| {
        if let Some(ui) = ui.upgrade()
            && p.orientation() == gtk::Orientation::Horizontal
            && p.position() > 0
        {
            ui.panel_width.set(p.position());
        }
    });

    // Dialogs adapt to their own width.
    let bp = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 560sp").expect("valid condition"),
    );
    w.device.narrow_layout(&bp);
    w.device_dialog.add_breakpoint(bp);
}

/// A dialog for one of the device's pages, with a header to close it by.
fn page_dialog(
    title: &str,
    content: &impl IsA<gtk::Widget>,
    width: i32,
    height: i32,
) -> adw::Dialog {
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(content));
    adw::Dialog::builder()
        .title(title)
        .content_width(width)
        .content_height(height)
        .child(&view)
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
        .default_width(1200)
        .default_height(820)
        .width_request(360)
        .height_request(294)
        .build();
    let saved = load_window_state();
    if let Some(st) = &saved {
        window.set_default_size(st.width, st.height);
        if st.maximized {
            window.maximize();
        }
    }
    let toasts = adw::ToastOverlay::new();
    let ctx = Rc::new(Ctx::new(cmds, window.clone(), toasts.clone()));

    let title = adw::WindowTitle::new("DP100 Studio", "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();

    let menu = gio::Menu::new();
    let pages_section = gio::Menu::new();
    pages_section.append(Some("_Presets"), Some("win.page::presets"));
    pages_section.append(Some("_Sequences"), Some("win.page::sequences"));
    pages_section.append(Some("_Recording"), Some("win.page::log"));
    pages_section.append(Some("_Device Settings"), Some("win.page::device"));
    menu.append_section(None, &pages_section);
    let device_section = gio::Menu::new();
    device_section.append(Some("Turn Output _Off"), Some("win.output-off"));
    device_section.append(Some("_Release Front Panel"), Some("win.release"));
    menu.append_section(None, &device_section);
    let app_section = gio::Menu::new();
    app_section.append(Some("_Keyboard Shortcuts"), Some("win.show-help-overlay"));
    app_section.append(Some("_About DP100 Studio"), Some("win.about"));
    menu.append_section(None, &app_section);
    header.pack_end(
        &gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build(),
    );

    let banner = adw::Banner::new("");

    let reconnect = gtk::Button::builder()
        .label("Reconnect")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    let setup_help = gtk::Button::builder()
        .label("Open Setup Guide")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    setup_help.connect_clicked(|b| {
        gtk::UriLauncher::new("https://github.com/slastra/dp100-studio#device-access").launch(
            b.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    let status_actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .build();
    status_actions.append(&reconnect);
    status_actions.append(&setup_help);
    let status = adw::StatusPage::builder()
        .icon_name("uninterruptible-power-supply-symbolic")
        .title("No DP100 Connected")
        .description(USBD_HINT)
        .child(&status_actions)
        .build();

    let controls = Controls::new(ctx.clone());
    let readouts = Readouts::new(controls);
    let presets = Presets::new(ctx.clone());
    let device = Device::new(ctx.clone(), simulated);
    let log = Log::new(ctx.clone());
    let sequences = Sequences::new(ctx.clone());
    header.pack_end(&log.header_record);

    // The panel: readings and setpoints, a running sequence, the preset.
    readouts.add_under_readings(&sequences.card);
    readouts.add_under_setpoints(&presets.recall);
    let panel = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(readouts::PANEL_WIDTH)
        .child(&readouts.pane)
        .build();
    let panel_width = saved.as_ref().and_then(|s| s.panel).unwrap_or(400);
    let paned = gtk::Paned::builder()
        .start_child(&panel)
        .end_child(&readouts.graph_area)
        .resize_start_child(false)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .position(panel_width)
        .build();

    // Each dialog's page lays itself out by the width it gets.
    let page = |child: &gtk::Widget, narrow_below: &str| {
        let bin = adw::BreakpointBin::builder()
            .width_request(340)
            .height_request(150)
            .child(child)
            .build();
        let bp = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(&format!("max-width: {narrow_below}"))
                .expect("valid condition"),
        );
        bin.add_breakpoint(bp.clone());
        (bin, bp)
    };
    let (presets_page, bp) = page(presets.root.widget().upcast_ref(), "600sp");
    presets.narrow_layout(&bp);
    let (sequences_page, bp) = page(sequences.root.widget().upcast_ref(), "700sp");
    sequences.narrow_layout(&bp);
    let (log_page, bp) = page(log.root.widget().upcast_ref(), "600sp");
    log.narrow_layout(&bp);
    let presets_dialog = page_dialog("Presets", &presets_page, 860, 720);
    let sequences_dialog = page_dialog("Sequences", &sequences_page, 1100, 800);
    let log_dialog = page_dialog("Recording", &log_page, 760, 560);
    let device_dialog = adw::PreferencesDialog::builder()
        .title("Device Settings")
        .width_request(340)
        .height_request(294)
        .build();
    device_dialog.add(&device.root);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_named(&status, Some("status"));
    stack.add_named(&paned, Some("device"));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&banner);
    view.set_content(Some(&stack));
    toasts.set_child(Some(&view));
    window.set_content(Some(&toasts));

    Window {
        ctx,
        title,
        paned,
        panel_width: std::cell::Cell::new(panel_width),
        presets_dialog,
        sequences_dialog,
        log_dialog,
        device_dialog,
        // A development hook for screenshots: open a page once connected.
        open_on_connect: RefCell::new(std::env::var("DP100_STUDIO_PAGE").ok()),
        connected: std::cell::Cell::new(false),
        banner,
        stack,
        status,
        reconnect,
        setup_help,
        readouts,
        presets,
        device,
        log,
        sequences,
        simulated,
        inhibit: std::cell::Cell::new(None),
        recording: std::cell::Cell::new(false),
        last_fault: std::cell::Cell::new(Fault::Ok),
        debug_info: RefCell::new(String::new()),
    }
}

impl Window {
    /// Shows the status page in place of the device pages.
    fn status(&self, title: &str, description: &str) {
        self.status.set_title(title);
        self.status.set_description(Some(description));
        self.reconnect.set_visible(false);
        self.setup_help.set_visible(false);
        self.set_banner(None, false);
        self.connected.set(false);
        self.update_header();
        if let Some(open) = self.ctx.window.visible_dialog() {
            open.close();
        }
        self.stack.set_visible_child_name("status");
    }

    /// The header names the app, and says when the DP100 is simulated.
    fn update_header(&self) {
        self.title
            .set_subtitle(if self.simulated { "Simulated" } else { "" });
    }

    /// Opens one of the device's pages: a dialog, or for "monitor", the main
    /// window with no dialog over it.
    fn show_page(&self, name: &str) {
        if !self.connected.get() {
            return;
        }
        if let Some(open) = self.ctx.window.visible_dialog() {
            open.close();
        }
        let dialog: Option<&adw::Dialog> = match name {
            "presets" => Some(&self.presets_dialog),
            "sequences" => Some(&self.sequences_dialog),
            "log" => Some(&self.log_dialog),
            "device" => Some(self.device_dialog.upcast_ref()),
            _ => None,
        };
        if let Some(d) = dialog {
            d.present(Some(&self.ctx.window));
        }
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
            Event::Searching => self.status("No DP100 Connected", USBD_HINT),
            Event::Power(st) => {
                let was_holding = self.readouts.is_holding();
                self.readouts.show_power(&st);
                if !was_holding {
                    self.update_inhibit();
                }
            }
            Event::PowerEnded(why) => {
                self.readouts.power_ended();
                self.update_inhibit();
                // Ending it yourself needs no word; anything else stopping
                // the hold does.
                if !matches!(
                    why.as_str(),
                    "Back to voltage mode." | "You turned the output off."
                ) {
                    self.ctx.toast(&format!("Power mode stopped. {why}"));
                    self.notify("power", "Power Mode Stopped", &why, "monitor");
                }
            }
            Event::Problem(p) => match &p {
                Problem::PermissionDenied(path) => {
                    self.status(
                        "Permission Needed",
                        &format!(
                            "This user can't open {}. Install the udev rule, then unplug the DP100 and plug it back in",
                            path.display()
                        ),
                    );
                    self.setup_help.set_visible(true);
                }
                Problem::Busy(path) => self.status(
                    "DP100 in Use",
                    &format!(
                        "Another program has {} open. DP100 Studio connects as soon as it closes",
                        path.display()
                    ),
                ),
                Problem::Other(e) => self.status("Can't Open the DP100", e),
            },
            Event::Connected(s) => {
                self.set_banner(None, false);
                self.connected.set(true);
                self.update_header();
                self.ctx.live.borrow_mut().output = Some(s.output);
                self.ctx.live.borrow_mut().system = Some(s.system);
                self.readouts.show_output(&s.output);
                self.presets.show_active(s.output.index);
                self.presets.show_output(&s.output);
                self.device.show_info(&s.info);
                *self.debug_info.borrow_mut() = format!(
                    "{} firmware {} hardware {} bootloader {} serial {}",
                    s.info.model,
                    s.info.firmware,
                    s.info.hardware,
                    s.info.bootloader,
                    s.info.serial_hex()
                );
                self.device.show_output(&s.output);
                self.device.show_system(&s.system);
                self.stack.set_visible_child_name("device");
                if let Some(name) = self.open_on_connect.take() {
                    self.show_page(&name);
                }
            }
            Event::Sample(s) => {
                let m = s.m;
                self.ctx.live.borrow_mut().sample = Some(m);
                self.readouts.show_sample(s.at, &m);
                self.device.show_sample(&m);
                if m.fault != Fault::Ok && self.last_fault.get() == Fault::Ok {
                    self.notify(
                        "trip",
                        "Protection Tripped",
                        &format!("{}: the DP100 turned its output off", m.fault.label()),
                        "monitor",
                    );
                }
                self.last_fault.set(m.fault);
                if m.fault != Fault::Ok {
                    self.set_banner(
                        Some(&format!(
                            "Protection tripped ({}). The output stays off until the trip is cleared",
                            m.fault.label()
                        )),
                        true,
                    );
                } else if m.mode == Mode::NoInput {
                    self.set_banner(
                        Some("No input power. Connect a USB-C PD charger or a 5–32 V DC supply"),
                        false,
                    );
                } else {
                    self.set_banner(None, false);
                }
            }
            Event::Output(o) => {
                let changed = self
                    .ctx
                    .live
                    .borrow()
                    .output
                    .is_none_or(|old| (old.ovp_mv, old.ocp_ma) != (o.ovp_mv, o.ocp_ma));
                self.ctx.live.borrow_mut().output = Some(o);
                if changed {
                    self.sequences.refresh_checks();
                }
                self.readouts.show_output(&o);
                self.presets.show_active(o.index);
                self.presets.show_output(&o);
                self.device.show_output(&o);
            }
            Event::Presets(p) => {
                self.ctx.live.borrow_mut().presets = p.clone();
                self.presets.show_presets(&p);
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
                if !self.recording.replace(true) {
                    self.update_inhibit();
                }
                self.log.show_recording(&r);
            }
            Event::RecordingStopped(r) => {
                self.recording.set(false);
                self.update_inhibit();
                if let Err(e) = &r {
                    self.notify("recording", "Recording Stopped", e, "log");
                }
                self.log.show_stopped(&r);
            }
            Event::Sequence { name, status } => {
                self.readouts.set_locked(true);
                let was_running = self.sequences.is_running();
                self.sequences.show_progress(&name, &status);
                if !was_running {
                    self.update_inhibit();
                }
            }
            Event::SequenceEnded { name, outcome } => {
                self.readouts.set_locked(false);
                self.sequences.show_ended(&name, &outcome);
                self.update_inhibit();
                match &outcome {
                    crate::seq::exec::Outcome::Finished => self.notify(
                        "sequence",
                        "Sequence Finished",
                        &format!("“{name}” ran to the end"),
                        "sequences",
                    ),
                    crate::seq::exec::Outcome::Aborted(why) => self.notify(
                        "sequence",
                        "Sequence Stopped",
                        &format!("“{name}”: {why}"),
                        "sequences",
                    ),
                    crate::seq::exec::Outcome::Running => {}
                }
            }
            Event::Done(Ok(Some(note))) => self.ctx.toast(&note),
            Event::Done(Ok(None)) => {}
            Event::Done(Err(e)) => {
                self.readouts.refused();
                self.ctx.error(&e);
            }
            Event::Released => {
                self.status(
                    "Front Panel Released",
                    "The DP100's buttons work again, and other programs can open it. \
                     DP100 Studio isn't reading the supply until you reconnect",
                );
                self.reconnect.set_visible(true);
            }
            Event::Disconnected(why) => self.status(
                "DP100 Disconnected",
                &format!("{why}. Reconnect it and put it in USBD mode: double-tap ◀ on its panel"),
            ),
        }
    }
}
