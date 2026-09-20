mod application;
mod config;
mod window;

use gettextrs::{LocaleCategory, bindtextdomain, setlocale, textdomain};
use gtk::prelude::*;
use gtk::{gio, glib};

use self::application::ObscureApplication;
use self::config::{APP_ID, GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR, PROFILE, VERSION};

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "obscure=info".into()),
        )
        .init();

    tracing::info!("Obscure {VERSION} ({PROFILE}), app id {APP_ID}");

    let resources = load_resources();

    // Safe: called on the main thread before any other thread is spawned.
    unsafe { setlocale(LocaleCategory::LcAll, "") };
    bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR).expect("unable to bind the text domain");
    textdomain(GETTEXT_PACKAGE).expect("unable to switch to the text domain");

    glib::set_application_name("Obscure");

    gio::resources_register(&resources);

    let app = ObscureApplication::new();
    app.run()
}

/// Loads the compiled gresource bundle.
///
/// The installed location (`PKGDATADIR`) is tried first. In the development
/// profile we fall back to the meson build tree so the binary can be run
/// directly from `build/` without `meson install`; in that case the schema
/// directory is pointed to the build tree as well.
fn load_resources() -> gio::Resource {
    let installed = std::path::Path::new(PKGDATADIR).join("obscure.gresource");
    if installed.exists() {
        return gio::Resource::load(&installed).expect("could not load installed resources");
    }

    let build_datadir = config::BUILD_DATADIR;
    if PROFILE == "development" && !build_datadir.is_empty() {
        let bundle = std::path::Path::new(build_datadir).join("resources/obscure.gresource");
        if bundle.exists() {
            tracing::warn!(
                "running uninstalled: using resources from {}",
                build_datadir
            );
            if std::env::var_os("GSETTINGS_SCHEMA_DIR").is_none() {
                // Safe: called from the main thread before any other thread exists.
                unsafe { std::env::set_var("GSETTINGS_SCHEMA_DIR", build_datadir) };
            }
            return gio::Resource::load(&bundle).expect("could not load build-tree resources");
        }
    }

    panic!(
        "could not find obscure.gresource in {} (did you run `meson install`?)",
        installed.display()
    );
}
