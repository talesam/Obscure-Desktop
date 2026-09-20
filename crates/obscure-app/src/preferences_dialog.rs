//! Preferences: background mode (GSettings) and connection settings
//! (stored in profiles.json through the ConnectionManager).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;

use crate::connection::ConnectionManager;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/preferences-dialog.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub background_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub autostart_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub connect_on_start_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub port_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub dns_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub prerelease_row: TemplateChild<adw::SwitchRow>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "ObscurePreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {}
    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl PreferencesDialog {
    pub fn new(manager: &ConnectionManager, settings: &gio::Settings, has_tray: bool) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.manager.set(manager.clone()).expect("set once");

        settings
            .bind("run-in-background", &*imp.background_row, "active")
            .build();
        // Without a tray there is no way back to the window: disable the option.
        imp.background_row.set_sensitive(has_tray);
        settings
            .bind("connect-on-start", &*imp.connect_on_start_row, "active")
            .build();
        // Autostart goes through the Background portal, which may refuse;
        // the switch reflects the stored value and requests on toggle.
        imp.autostart_row.set_active(settings.boolean("autostart"));
        let settings_clone = settings.clone();
        imp.autostart_row.connect_active_notify(clone!(
            #[weak]
            dialog,
            move |row| {
                let enable = row.is_active();
                if enable == settings_clone.boolean("autostart") {
                    return;
                }
                let settings = settings_clone.clone();
                let task = crate::runtime::runtime().spawn(crate::autostart::request(enable));
                glib::spawn_future_local(clone!(
                    #[weak]
                    dialog,
                    async move {
                        match task.await {
                            Ok(Ok(granted)) => {
                                let _ = settings.set_boolean("autostart", granted);
                                dialog.imp().autostart_row.set_active(granted);
                                if enable && !granted {
                                    dialog.add_toast(adw::Toast::new(&gettextrs::gettext(
                                        "The system did not allow Obscure to start automatically.",
                                    )));
                                }
                            }
                            Ok(Err(e)) => {
                                tracing::warn!("background portal: {e}");
                                dialog
                                    .imp()
                                    .autostart_row
                                    .set_active(settings.boolean("autostart"));
                                dialog.add_toast(adw::Toast::new(&gettextrs::gettext(
                                    "Automatic start is not available on this system.",
                                )));
                            }
                            Err(_) => {}
                        }
                    }
                ));
            }
        ));

        let profiles = manager.profiles();
        imp.port_row.set_value(f64::from(profiles.local_port));
        imp.dns_row.set_text(&profiles.dns);
        imp.prerelease_row.set_active(profiles.prerelease);

        // Port and pre-release apply immediately; DNS applies on ✓ or when
        // the dialog closes (EntryRow apply button).
        imp.port_row.connect_value_notify(clone!(
            #[weak]
            dialog,
            move |_| dialog.push_settings()
        ));
        imp.prerelease_row.connect_active_notify(clone!(
            #[weak]
            dialog,
            move |_| dialog.push_settings()
        ));
        imp.dns_row.connect_apply(clone!(
            #[weak]
            dialog,
            move |_| dialog.push_settings()
        ));
        dialog.connect_closed(|d| d.push_settings());
        dialog
    }

    fn push_settings(&self) {
        let imp = self.imp();
        let Some(m) = imp.manager.get() else { return };
        let port = imp.port_row.value().clamp(1025.0, 65535.0) as u16;
        m.update_settings(port, &imp.dns_row.text(), imp.prerelease_row.is_active());
    }
}
