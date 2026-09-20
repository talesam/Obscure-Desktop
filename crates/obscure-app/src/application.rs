use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::config::{APP_ID, PROFILE, VERSION};
use crate::connection::ConnectionManager;
use crate::tray::{ObscureTray, TrayCmd};
use crate::window::ObscureWindow;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ObscureApplication {
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub recovered: std::cell::Cell<bool>,
        pub tray: std::cell::RefCell<Option<ksni::Handle<ObscureTray>>>,
        pub settings: std::cell::OnceCell<gio::Settings>,
        /// Keeps the app alive while a tray icon exists.
        pub hold_guard: std::cell::RefCell<Option<gio::ApplicationHoldGuard>>,
        /// `--start-minimized`: do not show the window if a tray exists.
        pub start_minimized: std::cell::Cell<bool>,
        pub first_activation: std::cell::Cell<bool>,
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
            self.settings
                .set(gio::Settings::new(APP_ID))
                .expect("settings set once");
            self.obj().handle_termination_signals();
            self.obj().setup_tray();
            self.obj().setup_notifications();
            self.first_activation.set(true);
            self.obj().manager().refresh_due_subscriptions();
            if self
                .settings
                .get()
                .is_some_and(|s| s.boolean("connect-on-start"))
            {
                self.obj().manager().connect();
            }
        }

        fn handle_local_options(
            &self,
            options: &glib::VariantDict,
        ) -> std::ops::ControlFlow<glib::ExitCode> {
            if options.contains("start-minimized") {
                self.start_minimized.set(true);
            }
            self.parent_handle_local_options(options)
        }

        fn activate(&self) {
            let app = self.obj();
            let first = self.first_activation.replace(false);
            if first && self.start_minimized.get() && self.tray.borrow().is_some() {
                tracing::info!("started minimized: window stays hidden");
                return;
            }
            let window = match app.active_window() {
                Some(window) => window,
                None => {
                    let win = ObscureWindow::new(&*app, app.manager());
                    app.apply_background_policy(&win);
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

        /// `obscure vless://…` or clicking a share link in a browser.
        fn open(&self, files: &[gio::File], _hint: &str) {
            let app = self.obj();
            app.activate();
            let text: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
            if let Some(win) = app.active_window().and_downcast::<ObscureWindow>() {
                win.import_text(&text.join("\n"));
            }
        }

        fn shutdown(&self) {
            if let Some(m) = self.manager.get() {
                m.shutdown();
            }
            if let Some(tray) = self.tray.borrow_mut().take() {
                // Unregister from the watcher so the icon disappears at once.
                crate::runtime::runtime().block_on(async {
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(1), tray.shutdown())
                            .await;
                });
            }
            self.hold_guard.borrow_mut().take();
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
        let app: Self = glib::Object::builder()
            .property("application-id", APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", "/io/github/talesam/Obscure")
            .build();
        app.add_main_option(
            "start-minimized",
            glib::Char::from(0),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            "Start hidden in the tray (used by autostart)",
            None,
        );
        app
    }

    /// Desktop notifications for connection events. Routine events are
    /// only shown while the window is hidden; failures always.
    fn setup_notifications(&self) {
        let app = self.clone();
        self.manager().connect_closure(
            "notification",
            false,
            glib::closure_local!(move |_m: ConnectionManager,
                                       title: String,
                                       body: String,
                                       important: bool| {
                let window_visible = app
                    .active_window()
                    .is_some_and(|w| w.is_visible() && w.is_active());
                if !important && window_visible {
                    return;
                }
                let notification = gio::Notification::new(&title);
                notification.set_body(Some(&body));
                notification.set_icon(&gio::ThemedIcon::new(APP_ID));
                notification.set_default_action("app.activate-window");
                app.send_notification(Some("connection-state"), &notification);
            }),
        );
        let activate = gio::ActionEntry::builder("activate-window")
            .activate(|app: &Self, _, _| app.activate())
            .build();
        self.add_action_entries([activate]);
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

    /// SIGTERM/SIGINT/SIGHUP (logout, `kill`, Ctrl+C) go through the normal
    /// shutdown path so the system proxy is restored and Xray is stopped.
    /// Signals are caught on the tokio runtime and forwarded to the main loop.
    fn handle_termination_signals(&self) {
        use tokio::signal::unix::{SignalKind, signal};
        let (tx, rx) = async_channel::bounded::<i32>(1);
        crate::runtime::runtime().spawn(async move {
            let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
            let mut hup = signal(SignalKind::hangup()).expect("SIGHUP handler");
            let signum = tokio::select! {
                _ = term.recv() => 15,
                _ = int.recv() => 2,
                _ = hup.recv() => 1,
            };
            let _ = tx.send(signum).await;
        });
        let app = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(signum) = rx.recv().await {
                tracing::info!("signal {signum} received, shutting down cleanly");
                app.quit();
            }
        });
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
        let has_tray = self.imp().tray.borrow().is_some();
        crate::preferences_dialog::PreferencesDialog::new(
            self.manager(),
            self.settings(),
            has_tray,
        )
        .present(self.active_window().as_ref());
    }

    fn settings(&self) -> &gio::Settings {
        self.imp()
            .settings
            .get()
            .expect("settings exist after startup")
    }

    /// Whether closing the window should keep the app alive in the tray.
    fn runs_in_background(&self) -> bool {
        self.imp().tray.borrow().is_some() && self.settings().boolean("run-in-background")
    }

    /// Hides instead of destroying the window when running in background,
    /// and follows the preference live.
    fn apply_background_policy(&self, win: &ObscureWindow) {
        win.set_hide_on_close(self.runs_in_background());
        let app = self.clone();
        let win_weak = win.downgrade();
        self.settings()
            .connect_changed(Some("run-in-background"), move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.set_hide_on_close(app.runs_in_background());
                }
            });
    }

    /// Registers the tray icon (if a host exists) and keeps the app alive
    /// while it is shown. Tray commands are forwarded to the main loop.
    fn setup_tray(&self) {
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<TrayCmd>();
        let (handle_tx, handle_rx) = async_channel::bounded::<Option<ksni::Handle<ObscureTray>>>(1);
        crate::runtime::runtime().spawn(async move {
            let handle = crate::tray::spawn(cmd_tx).await;
            let _ = handle_tx.send(handle).await;
        });

        let app = self.clone();
        glib::spawn_future_local(async move {
            let Ok(Some(handle)) = handle_rx.recv().await else {
                return;
            };
            tracing::info!("tray icon registered");
            *app.imp().tray.borrow_mut() = Some(handle);
            if app.imp().hold_guard.borrow().is_none() {
                *app.imp().hold_guard.borrow_mut() = Some(app.hold());
            }
            if let Some(win) = app.active_window().and_downcast::<ObscureWindow>() {
                app.apply_background_policy(&win);
            }
            app.mirror_state_to_tray();

            while let Ok(cmd) = cmd_rx.recv().await {
                match cmd {
                    TrayCmd::ShowWindow => app.activate(),
                    TrayCmd::ToggleConnection => app.manager().toggle(),
                    TrayCmd::Quit => app.quit(),
                }
            }
        });
    }

    /// Pushes connection state changes to the tray icon and menu.
    fn mirror_state_to_tray(&self) {
        let m = self.manager();
        let update = {
            let app = self.clone();
            move |m: &ConnectionManager| {
                let Some(handle) = app.imp().tray.borrow().clone() else {
                    return;
                };
                let (connected, busy, server, status) =
                    (m.connected(), m.busy(), m.selected_name(), m.status_text());
                crate::runtime::runtime().spawn(async move {
                    handle
                        .update(|t| {
                            t.connected = connected;
                            t.busy = busy;
                            t.server_name = server;
                            t.status_text = status;
                        })
                        .await;
                });
            }
        };
        m.connect_connected_notify(update.clone());
        m.connect_busy_notify(update.clone());
        m.connect_selected_name_notify(update.clone());
        m.connect_status_text_notify(update.clone());
        update(m);
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
            .comments(format!(
                "{}\n\n{}",
                gettext("Connect. That's it."),
                gettext("Obscure uses Xray-core, downloaded separately on first run.")
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
