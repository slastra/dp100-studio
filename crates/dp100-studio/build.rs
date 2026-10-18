//! Compiles the app's resources (icons that not every theme has) into the
//! binary.

fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "dp100-studio.gresource",
    );
}
