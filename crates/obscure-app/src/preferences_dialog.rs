//! Preferences: background mode (GSettings) and connection settings
//! (stored in profiles.json through the ConnectionManager).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gio;
use gtk::glib;
use gtk::glib::clone;

use gettextrs::gettext;
use obscure_core::profile::{RouteRule, RuleTarget};

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
        #[template_child]
        pub shortcut_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub rules_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub add_rule_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub rules_list: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub json_row: TemplateChild<adw::ActionRow>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub rules: std::cell::RefCell<Vec<RouteRule>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "ObscurePreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl PreferencesDialog {
        #[template_callback]
        fn on_add_rule_clicked(&self, _b: &gtk::Button) {
            self.obj().show_add_rule();
        }

        #[template_callback]
        fn on_json_activated(&self, _row: &adw::ActionRow) {
            if let Some(m) = self.manager.get() {
                crate::json_dialog::JsonDialog::new(m).present(Some(&*self.obj()));
            }
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

        // Advanced: global shortcut (GSettings), route rules, JSON editor.
        settings
            .bind("global-shortcut", &*imp.shortcut_row, "active")
            .build();
        *imp.rules.borrow_mut() = profiles.custom_rules.clone();
        dialog.rebuild_rules();
        dialog
    }

    fn rebuild_rules(&self) {
        let imp = self.imp();
        while let Some(child) = imp.rules_list.first_child() {
            imp.rules_list.remove(&child);
        }
        let rules = imp.rules.borrow().clone();
        for (i, rule) in rules.iter().enumerate() {
            let row = adw::ActionRow::builder()
                .title(&rule.pattern)
                .subtitle(target_label(rule.target))
                .build();
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text(gettext("Remove rule"))
                .build();
            remove.connect_clicked(clone!(
                #[weak(rename_to = dialog)]
                self,
                move |_| {
                    dialog.imp().rules.borrow_mut().remove(i);
                    dialog.commit_rules();
                }
            ));
            row.add_suffix(&remove);
            imp.rules_list.append(&row);
        }
        imp.rules_list.set_visible(!rules.is_empty());
    }

    fn commit_rules(&self) {
        let rules = self.imp().rules.borrow().clone();
        if let Some(m) = self.imp().manager.get() {
            m.set_custom_rules(rules);
        }
        self.rebuild_rules();
    }

    fn show_add_rule(&self) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("New rule"))
            .body(gettext("What should match, and where it goes."))
            .build();
        let list = gtk::ListBox::builder().css_classes(["boxed-list"]).build();
        let entry = adw::EntryRow::builder()
            .title(gettext("Domain, address or category"))
            .build();
        let targets = gtk::StringList::new(&[
            &gettext("Through the server"),
            &gettext("Direct"),
            &gettext("Block"),
        ]);
        let combo = adw::ComboRow::builder()
            .title(gettext("Send to"))
            .model(&targets)
            .build();
        list.append(&entry);
        list.append(&combo);
        dialog.set_extra_child(Some(&list));
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("add", &gettext("Add"))]);
        dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("add"));
        dialog.connect_response(
            Some("add"),
            clone!(
                #[weak(rename_to = prefs)]
                self,
                move |_, _| {
                    let pattern = entry.text().trim().to_owned();
                    if pattern.is_empty() || pattern.contains(char::is_whitespace) {
                        return;
                    }
                    let target = match combo.selected() {
                        0 => RuleTarget::Proxy,
                        1 => RuleTarget::Direct,
                        _ => RuleTarget::Block,
                    };
                    prefs
                        .imp()
                        .rules
                        .borrow_mut()
                        .push(RouteRule { pattern, target });
                    prefs.commit_rules();
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn push_settings(&self) {
        let imp = self.imp();
        let Some(m) = imp.manager.get() else { return };
        let port = imp.port_row.value().clamp(1025.0, 65535.0) as u16;
        m.update_settings(port, &imp.dns_row.text(), imp.prerelease_row.is_active());
    }
}

fn target_label(t: RuleTarget) -> String {
    match t {
        RuleTarget::Proxy => gettext("Through the server"),
        RuleTarget::Direct => gettext("Direct"),
        RuleTarget::Block => gettext("Block"),
    }
}
