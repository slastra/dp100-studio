mod logging;
mod seq;
mod ui;
mod worker;

use adw::prelude::*;

pub const APP_ID: &str = "io.github.slastra.DP100Studio";

fn main() -> gtk::glib::ExitCode {
    // `--simulate` runs against a software DP100. It is taken out before GTK
    // sees the arguments, since GApplication rejects options it doesn't know.
    let mut args: Vec<String> = std::env::args().collect();
    let simulate = args.iter().any(|a| a == "--simulate");
    args.retain(|a| a != "--simulate");

    // Icons that not every theme has, found when the one in use lacks them.
    gtk::gio::resources_register_include!("dp100-studio.gresource")
        .expect("the resources are built into the binary");

    let app = adw::Application::builder()
        .application_id(if simulate {
            // A separate id, so a simulated window can run beside the real one.
            "io.github.slastra.DP100Studio.Simulated"
        } else {
            APP_ID
        })
        // Set rather than taken from the id, so the simulated one finds the
        // same icons: GTK adds this path's "icons" to the icon search path.
        .resource_base_path("/io/github/slastra/DP100Studio")
        .build();
    app.connect_activate(move |app| ui::activate(app, simulate));
    // A terminal Ctrl-C or a session ending quits cleanly, which closes the
    // window, stops the worker, and finishes any recording, rather than
    // dying with up to a second of rows still buffered.
    for signal in [2, 15] {
        let app = app.clone();
        glib_unix::unix_signal_add_local(signal, move || {
            app.quit();
            gtk::glib::ControlFlow::Break
        });
    }
    app.run_with_args(&args)
}
