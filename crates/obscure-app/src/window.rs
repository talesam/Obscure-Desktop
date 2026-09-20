use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::glib::clone;
use gtk::{gio, glib};
use obscure_core::links::looks_like_link;
use obscure_core::sysproxy::env_snippet;

use crate::config::APP_ID;
use crate::connection::ConnectionManager;
use crate::import_dialog::{ImportDialog, ServersBox};
use crate::log_dialog::LogDialog;
use crate::server_dialog::ServerDialog;
use crate::server_object::ServerObject;
use crate::subscription_object::SubscriptionObject;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/window.ui")]
    pub struct ObscureWindow {
        #[template_child]
        pub toolbar_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub server_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub traffic_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub connect_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub connect_content: TemplateChild<adw::ButtonContent>,
        #[template_child]
        pub mode_group: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub route_group: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub servers_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub subscriptions_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub subscriptions_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub test_all_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub auto_row: TemplateChild<adw::SwitchRow>,

        pub manager: std::cell::OnceCell<ConnectionManager>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ObscureWindow {
        const NAME: &'static str = "ObscureWindow";
        type Type = super::ObscureWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
            klass.install_action("win.add-server", None, |win, _, _| {
                win.show_import_dialog(None)
            });
            klass.install_action("win.copy-env", None, |win, _, _| win.copy_env());
            klass.install_action("win.show-log", None, |win, _, _| win.show_log());
            klass.install_action("win.show-connections", None, |win, _, _| {
                win.show_connections()
            });
            klass.install_action("win.import-file", None, |win, _, _| win.import_from_file());
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl ObscureWindow {
        #[template_callback]
        fn on_connect_clicked(&self, _button: &gtk::Button) {
            self.obj().manager().toggle();
        }

        #[template_callback]
        fn on_mode_changed(&self, _pspec: glib::ParamSpec, _group: &adw::ToggleGroup) {
            if let Some(name) = self.mode_group.active_name() {
                self.obj().manager().set_apply_mode_from_ui(&name);
            }
        }

        #[template_callback]
        fn on_route_changed(&self, _pspec: glib::ParamSpec, _group: &adw::ToggleGroup) {
            if let Some(name) = self.route_group.active_name() {
                self.obj().manager().set_route_preset_from_ui(&name);
            }
        }

        #[template_callback]
        fn on_test_all_clicked(&self, _b: &gtk::Button) {
            self.obj().manager().test_all_latency(|_| {});
        }

        #[template_callback]
        fn on_auto_toggled(&self, _pspec: glib::ParamSpec, row: &adw::SwitchRow) {
            self.obj()
                .manager()
                .set_auto_select_from_ui(row.is_active());
        }

        #[template_callback]
        fn on_server_activated(&self, row: &gtk::ListBoxRow, _list: &gtk::ListBox) {
            let Some(row) = row.downcast_ref::<adw::ActionRow>() else {
                return;
            };
            if let Some(id) = unsafe { row.data::<String>("server-id") } {
                let id = unsafe { id.as_ref() }.clone();
                self.obj().manager().select_server(&id);
            }
        }
    }

    impl ObjectImpl for ObscureWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.status_page
                .set_icon_name(Some(&format!("{APP_ID}-symbolic")));
            obj.bind_settings();
            obj.setup_paste_shortcut();
        }
    }

    impl WidgetImpl for ObscureWindow {}
    impl WindowImpl for ObscureWindow {}
    impl ApplicationWindowImpl for ObscureWindow {}
    impl AdwApplicationWindowImpl for ObscureWindow {}
}

glib::wrapper! {
    pub struct ObscureWindow(ObjectSubclass<imp::ObscureWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ObscureWindow {
    pub fn new<P: IsA<gtk::Application>>(application: &P, manager: &ConnectionManager) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();
        win.imp()
            .manager
            .set(manager.clone())
            .expect("manager set once");
        win.bind_manager();
        win
    }

    fn manager(&self) -> &ConnectionManager {
        self.imp().manager.get().expect("manager is set in new()")
    }

    /// Persists window geometry in the gschema.
    fn bind_settings(&self) {
        let settings = gio::Settings::new(APP_ID);
        settings.bind("window-width", self, "default-width").build();
        settings
            .bind("window-height", self, "default-height")
            .build();
        settings.bind("window-maximized", self, "maximized").build();
    }

    /// Ctrl+V anywhere in the window imports a link from the clipboard.
    fn setup_paste_shortcut(&self) {
        let controller = gtk::ShortcutController::new();
        controller.set_scope(gtk::ShortcutScope::Global);
        let action = gtk::CallbackAction::new(|widget, _| {
            let win = widget.downcast_ref::<ObscureWindow>().expect("window");
            // Do not steal Ctrl+V from text fields.
            if GtkWindowExt::focus(win)
                .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>())
            {
                return glib::Propagation::Proceed;
            }
            tracing::debug!("Ctrl+V shortcut triggered");
            win.import_from_clipboard();
            glib::Propagation::Stop
        });
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>v"),
            Some(action),
        ));
        self.add_controller(controller);
    }

    fn import_from_clipboard(&self) {
        let clipboard = self.clipboard();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                match clipboard.read_text_future().await {
                    Ok(Some(text))
                        if looks_like_link(&text)
                            || text.lines().any(looks_like_link)
                            || obscure_core::profile::looks_like_subscription_url(&text) =>
                    {
                        win.show_import_dialog(Some(&text));
                    }
                    _ => win.toast(&gettext("The clipboard does not contain a server link.")),
                }
            }
        ));
    }

    /// Opens the import dialog with text handed over by the system
    /// (URL scheme handler or file).
    pub fn import_text(&self, text: &str) {
        self.show_import_dialog(Some(text));
    }

    fn show_connections(&self) {
        crate::connections_dialog::ConnectionsDialog::new(self.manager()).present(Some(self));
    }

    fn show_log(&self) {
        LogDialog::new(self.manager()).present(Some(self));
    }

    fn import_from_file(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&gettext("Text files")));
        filter.add_mime_type("text/plain");
        filter.add_pattern("*.txt");
        filter.add_pattern("*.json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Import servers from a file"))
            .modal(true)
            .filters(&filters)
            .build();
        dialog.open(
            Some(self),
            gio::Cancellable::NONE,
            clone!(
                #[weak(rename_to = win)]
                self,
                move |result| {
                    let Ok(file) = result else { return };
                    glib::spawn_future_local(async move {
                        match file.load_contents_future().await {
                            Ok((bytes, _)) => {
                                let text = String::from_utf8_lossy(&bytes).into_owned();
                                win.show_import_dialog(Some(&text));
                            }
                            Err(e) => {
                                tracing::warn!("cannot read file: {e}");
                                win.toast(&gettext("Could not read that file."));
                            }
                        }
                    });
                }
            ),
        );
    }

    fn show_server_details(&self, id: &str) {
        let Some(dialog) = ServerDialog::new(self.manager(), id) else {
            return;
        };
        dialog.connect_closure(
            "remove-requested",
            false,
            glib::closure_local!(
                #[weak(rename_to = win)]
                self,
                move |_d: ServerDialog, id: String| {
                    let name = win
                        .manager()
                        .server_entry(&id)
                        .map(|e| e.server.name)
                        .unwrap_or_default();
                    win.confirm_remove(&id, &name);
                }
            ),
        );
        dialog.connect_closure(
            "toast",
            false,
            glib::closure_local!(
                #[weak(rename_to = win)]
                self,
                move |_d: ServerDialog, text: String| win.toast(&text)
            ),
        );
        dialog.present(Some(self));
    }

    fn show_import_dialog(&self, prefill: Option<&str>) {
        tracing::debug!("opening import dialog (prefilled: {})", prefill.is_some());
        let dialog = match prefill {
            Some(text) => ImportDialog::with_text(text),
            None => ImportDialog::new(),
        };
        dialog.connect_closure(
            "subscription-url",
            false,
            glib::closure_local!(
                #[weak(rename_to = win)]
                self,
                move |_dialog: ImportDialog, url: String| {
                    win.toast(&downloading_subscription_message());
                    win.manager().add_subscription_url(
                        url,
                        clone!(
                            #[weak]
                            win,
                            move |result| match result {
                                Ok(n) => win.toast(&added_message(n)),
                                Err(e) => win.toast(&crate::humanize::error_message(&e)),
                            }
                        ),
                    );
                }
            ),
        );
        dialog.connect_closure(
            "servers-parsed",
            false,
            glib::closure_local!(
                #[weak(rename_to = win)]
                self,
                move |_dialog: ImportDialog, servers: ServersBox| {
                    let added = win.manager().add_servers(servers.0);
                    win.toast(&added_message(added));
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn copy_env(&self) {
        self.clipboard()
            .set_text(&env_snippet(obscure_core::DEFAULT_LOCAL_PORT));
        self.toast(&gettext(
            "Environment variables copied. Paste them in a terminal.",
        ));
    }

    pub fn toast(&self, text: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(text));
    }

    /// Wires manager properties to widgets.
    fn bind_manager(&self) {
        let imp = self.imp();
        let m = self.manager();

        m.bind_property("selected_name", &*imp.server_label, "label")
            .sync_create()
            .build();
        m.bind_property("status_text", &*imp.status_label, "label")
            .sync_create()
            .build();
        m.bind_property("traffic_text", &*imp.traffic_label, "label")
            .sync_create()
            .build();
        m.bind_property("traffic_text", &*imp.traffic_label, "visible")
            .transform_to(|_, t: String| Some(!t.is_empty()))
            .sync_create()
            .build();
        m.bind_property("progress", &*imp.progress_bar, "fraction")
            .sync_create()
            .build();
        m.bind_property("progress_visible", &*imp.progress_bar, "visible")
            .sync_create()
            .build();
        m.bind_property("has_servers", &*imp.stack, "visible-child-name")
            .transform_to(|_, has: bool| Some(if has { "main" } else { "empty" }))
            .sync_create()
            .build();
        m.bind_property("apply_mode", &*imp.mode_group, "active-name")
            .sync_create()
            .build();
        m.bind_property("route_preset", &*imp.route_group, "active-name")
            .sync_create()
            .build();

        let update_button = clone!(
            #[weak(rename_to = win)]
            self,
            move |m: &ConnectionManager| win.update_connect_button(m)
        );
        m.connect_connected_notify(update_button.clone());
        m.connect_busy_notify(update_button.clone());
        update_button(m);

        m.connect_closure(
            "failed",
            false,
            glib::closure_local!(
                #[weak(rename_to = win)]
                self,
                move |m: ConnectionManager| {
                    if let Some(msg) = m.last_error() {
                        win.toast(&msg);
                    }
                }
            ),
        );

        imp.servers_list.bind_model(
            Some(m.servers()),
            clone!(
                #[weak(rename_to = win)]
                self,
                #[upgrade_or_panic]
                move |item| win.build_server_row(item)
            ),
        );
        imp.subscriptions_list.bind_model(
            Some(m.subscriptions()),
            clone!(
                #[weak(rename_to = win)]
                self,
                #[upgrade_or_panic]
                move |item| win.build_subscription_row(item)
            ),
        );
        m.bind_property("has_subscriptions", &*imp.subscriptions_group, "visible")
            .sync_create()
            .build();
        m.bind_property("auto_select", &*imp.auto_row, "active")
            .sync_create()
            .build();
        m.bind_property("testing", &*imp.test_all_button, "sensitive")
            .invert_boolean()
            .sync_create()
            .build();
    }

    fn build_subscription_row(&self, item: &glib::Object) -> gtk::Widget {
        let sub = item
            .downcast_ref::<SubscriptionObject>()
            .expect("SubscriptionObject");
        let row = adw::ActionRow::builder()
            .title(sub.name())
            .subtitle(sub.subtitle())
            .build();

        let bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(1.0)
            .valign(gtk::Align::Center)
            .width_request(72)
            .build();
        bar.add_offset_value(gtk::LEVEL_BAR_OFFSET_LOW, 0.8);
        bar.add_offset_value(gtk::LEVEL_BAR_OFFSET_HIGH, 0.95);
        bar.add_offset_value(gtk::LEVEL_BAR_OFFSET_FULL, 1.0);
        sub.bind_property("fraction", &bar, "value")
            .sync_create()
            .build();
        sub.bind_property("has_quota", &bar, "visible")
            .sync_create()
            .build();
        row.add_suffix(&bar);

        let spinner = adw::Spinner::new();
        sub.bind_property("updating", &spinner, "visible")
            .sync_create()
            .build();
        row.add_suffix(&spinner);

        let update = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(gettext("Update now"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        sub.bind_property("updating", &update, "visible")
            .invert_boolean()
            .sync_create()
            .build();
        let id = sub.id();
        update.connect_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                win.manager().update_subscription(
                    id.clone(),
                    clone!(
                        #[weak]
                        win,
                        move |result| match result {
                            Ok(n) => win.toast(&subscription_updated_message(n)),
                            Err(e) => win.toast(&crate::humanize::error_message(&e)),
                        }
                    ),
                );
            }
        ));
        row.add_suffix(&update);

        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(gettext("Remove subscription"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let id = sub.id();
        let name = sub.name();
        remove.connect_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.confirm_remove_subscription(&id, &name)
        ));
        row.add_suffix(&remove);
        row.upcast()
    }

    fn confirm_remove_subscription(&self, id: &str, name: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Remove “%s” and all its servers?").replace("%s", name))
            .body(gettext(
                "You can add the subscription again later by pasting its address.",
            ))
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("remove", &gettext("Remove")),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let id = id.to_owned();
        dialog.connect_response(
            Some("remove"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.manager().remove_subscription(&id)
            ),
        );
        dialog.present(Some(self));
    }

    fn update_connect_button(&self, m: &ConnectionManager) {
        let imp = self.imp();
        let (label, icon, status_icon, css_add, css_remove) = if m.connected() {
            (
                gettext("Disconnect"),
                "network-vpn-disabled-symbolic",
                "network-vpn-symbolic",
                "destructive-action",
                "suggested-action",
            )
        } else if m.busy() {
            (
                gettext("Cancel"),
                "process-stop-symbolic",
                "network-vpn-acquiring-symbolic",
                "destructive-action",
                "suggested-action",
            )
        } else {
            (
                gettext("Connect"),
                "network-vpn-symbolic",
                "network-vpn-disconnected-symbolic",
                "suggested-action",
                "destructive-action",
            )
        };
        imp.connect_content.set_label(&label);
        imp.connect_content.set_icon_name(icon);
        imp.status_icon.set_icon_name(Some(status_icon));
        imp.connect_button.remove_css_class(css_remove);
        imp.connect_button.add_css_class(css_add);
        if m.connected() {
            imp.status_icon.remove_css_class("dim-label");
            imp.status_icon.add_css_class("success");
        } else {
            imp.status_icon.remove_css_class("success");
            imp.status_icon.add_css_class("dim-label");
        }
    }

    fn build_server_row(&self, item: &glib::Object) -> gtk::Widget {
        let server = item.downcast_ref::<ServerObject>().expect("ServerObject");
        let row = adw::ActionRow::builder()
            .title(server.name())
            .subtitle(server.subtitle())
            .activatable(true)
            .build();
        // Stash the id on the row for `on_server_activated`.
        unsafe { row.set_data("server-id", server.id()) };

        let latency = gtk::Label::builder()
            .valign(gtk::Align::Center)
            .css_classes(["caption", "numeric", "obscure-latency"])
            .build();
        let update_badge = clone!(
            #[weak]
            latency,
            move |s: &ServerObject| match ServerObject::latency_badge(s.latency_ms()) {
                Some((text, class)) => {
                    latency.set_label(&text);
                    for c in ["success", "warning", "error", "dim-label"] {
                        latency.remove_css_class(c);
                    }
                    latency.add_css_class(class);
                    latency.set_visible(true);
                }
                None => latency.set_visible(false),
            }
        );
        server.connect_latency_ms_notify(update_badge.clone());
        update_badge(server);
        row.add_suffix(&latency);

        let check = gtk::Image::from_icon_name("object-select-symbolic");
        server
            .bind_property("selected", &check, "visible")
            .sync_create()
            .build();
        row.add_suffix(&check);

        let details = gtk::Button::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(gettext("Server details"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let id = server.id();
        details.connect_clicked(clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.show_server_details(&id)
        ));
        row.add_suffix(&details);
        row.upcast()
    }

    fn confirm_remove(&self, id: &str, name: &str) {
        tracing::debug!("confirm removal of {id}");
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Remove “%s”?").replace("%s", name))
            .body(gettext("You can add it again later by pasting its link."))
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("Cancel")),
            ("remove", &gettext("Remove")),
        ]);
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        let id = id.to_owned();
        dialog.connect_response(
            Some("remove"),
            clone!(
                #[weak(rename_to = win)]
                self,
                move |_, _| win.manager().remove_server(&id)
            ),
        );
        dialog.present(Some(self));
    }
}

/// Toast text after an import. Kept out of macros so xgettext sees it.
fn added_message(added: usize) -> String {
    if added == 0 {
        gettext("These servers were already added.")
    } else {
        ngettext("%n server added.", "%n servers added.", added as u32)
            .replace("%n", &added.to_string())
    }
}

fn downloading_subscription_message() -> String {
    gettext("Downloading the subscription…")
}

fn subscription_updated_message(n: usize) -> String {
    ngettext(
        "Subscription updated: %n server.",
        "Subscription updated: %n servers.",
        n as u32,
    )
    .replace("%n", &n.to_string())
}
