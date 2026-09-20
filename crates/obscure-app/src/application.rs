use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::config::{APP_ID, PROFILE, VERSION};
use crate::connection::ConnectionManager;
use crate::window::ObscureWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ObscureApplication {
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub recovered: std::cell::Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ObscureApplication {
        const NAME: &'static str = "ObscureApplication";
        type Type = super::ObscureApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for ObscureApplication {
        fn constructed(&self) {
            self.parent_constructed();
            let app = self.obj();
            app.setup_gactions();
            app.set_accels_for_action("app.quit", &["<primary>q"]);
            app.set_accels_for_action("app.preferences", &["<primary>comma"]);
            app.set_accels_for_action("app.shortcuts", &["<primary>question"]);
            app.set_accels_for_action("window.close", &["<primary>w"]);
        }
    }

    impl ApplicationImpl for ObscureApplication {
        fn startup(&self) {
            self.parent_startup();
            self.obj().setup_uninstalled_icons();
            self.obj().load_css();
            let manager = ConnectionManager::new();
            self.recovered.set(manager.recover_from_crash());
            self.manager.set(manager).expect("manager set once");
        }

        fn activate(&self) {
            let app = self.obj();
            let window = match app.active_window() {
                Some(window) => window,
                None => {
                    let win = ObscureWindow::new(&*app, app.manager());
                    if self.recovered.replace(false) {
                        win.toast(&gettext(
                            "The system proxy settings from a previous session were restored.",
                        ));
                    }
                    win.upcast()
                }
            };
            window.present();
        }

        fn shutdown(&self) {
            if let Some(m) = self.manager.get() {
                m.shutdown();
            }
            self.parent_shutdown();
        }
    }

    impl GtkApplicationImpl for ObscureApplication {}
    impl AdwApplicationImpl for ObscureApplication {}
}

glib::wrapper! {
    pub struct ObscureApplication(ObjectSubclass<imp::ObscureApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for ObscureApplication {
    fn default() -> Self {
        Self::new()
    }
}

impl ObscureApplication {
    pub fn manager(&self) -> &ConnectionManager {
        self.imp()
            .manager
            .get()
            .expect("manager exists after startup")
    }

    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", APP_ID)
            .property("flags", gio::ApplicationFlags::default())
            .property("resource-base-path", "/io/github/talesam/Obscure")
            .build()
    }

    /// When running from the meson build tree (development profile, not
    /// installed) the icons are not in the icon theme; add build/data/icons
    /// as an unthemed search path so the app icon still shows up.
    fn setup_uninstalled_icons(&self) {
        let build_datadir = crate::config::BUILD_DATADIR;
        if PROFILE != "development" || build_datadir.is_empty() {
            return;
        }
        let installed = std::path::Path::new(crate::config::PKGDATADIR).join("obscure.gresource");
        if installed.exists() {
            return;
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let icons = std::path::Path::new(build_datadir).join("icons");
            gtk::IconTheme::for_display(&display).add_search_path(icons);
        }
    }

    fn load_css(&self) {
        let provider = gtk::CssProvider::new();
        provider.load_from_resource("/io/github/talesam/Obscure/style.css");
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }

    fn setup_gactions(&self) {
        let quit = gio::ActionEntry::builder("quit")
            .activate(|app: &Self, _, _| app.quit())
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(|app: &Self, _, _| app.show_preferences())
            .build();
        let shortcuts = gio::ActionEntry::builder("shortcuts")
            .activate(|app: &Self, _, _| app.show_shortcuts())
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        self.add_action_entries([quit, preferences, shortcuts, about]);
    }

    fn show_preferences(&self) {
        let builder =
            gtk::Builder::from_resource("/io/github/talesam/Obscure/ui/preferences-dialog.ui");
        let dialog: adw::PreferencesDialog = builder
            .object("preferences_dialog")
            .expect("preferences-dialog.ui must define `preferences_dialog`");
        dialog.present(self.active_window().as_ref());
    }

    fn show_shortcuts(&self) {
        let builder =
            gtk::Builder::from_resource("/io/github/talesam/Obscure/ui/shortcuts-dialog.ui");
        let dialog: adw::ShortcutsDialog = builder
            .object("shortcuts_dialog")
            .expect("shortcuts-dialog.ui must define `shortcuts_dialog`");
        dialog.present(self.active_window().as_ref());
    }

    fn show_about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name("Obscure")
            .application_icon(APP_ID)
            .developer_name("Tales A. Mendonça")
            .version(VERSION)
            .website("https://github.com/talesam/Obscure-Desktop")
            .issue_url("https://github.com/talesam/Obscure-Desktop/issues")
            .license_type(gtk::License::Gpl30)
            .developers(["Tales A. Mendonça"])
            // Translators: replace with your name and, optionally, e-mail.
            .translator_credits(gettext("translator-credits"))
            .comments(gettext(
                "Connect. That's it.\n\nObscure uses Xray-core, downloaded separately on first run.",
            ))
            .build();

        if PROFILE == "development" {
            dialog.add_link(
                &gettext("Project plan"),
                "https://github.com/talesam/Obscure-Desktop/blob/main/docs/PLANO.md",
            );
        }

        dialog.present(self.active_window().as_ref());
    }
}
