//! Raw Xray configuration editor with `xray run -test` validation.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use gtk::glib::clone;
use obscure_core::paths;
use obscure_core::supervisor::{Supervisor, write_config};

use crate::connection::ConnectionManager;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/json-dialog.ui")]
    pub struct JsonDialog {
        #[template_child]
        pub validate_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub apply_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub result_label: TemplateChild<gtk::Label>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for JsonDialog {
        const NAME: &'static str = "ObscureJsonDialog";
        type Type = super::JsonDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl JsonDialog {
        #[template_callback]
        fn on_validate_clicked(&self, _b: &gtk::Button) {
            self.obj().validate(false);
        }

        #[template_callback]
        fn on_apply_clicked(&self, _b: &gtk::Button) {
            self.obj().validate(true);
        }

        #[template_callback]
        fn on_reset_clicked(&self, _b: &gtk::Button) {
            let obj = self.obj();
            if let Some(m) = self.manager.get() {
                m.set_config_override(None);
                if let Some(json) = m.generated_config_json() {
                    self.text_view.buffer().set_text(&json);
                }
            }
            obj.show_result(&gettext("Back to the automatic configuration."), false);
            self.reset_button.set_sensitive(false);
        }
    }

    impl ObjectImpl for JsonDialog {}
    impl WidgetImpl for JsonDialog {}
    impl AdwDialogImpl for JsonDialog {}
}

glib::wrapper! {
    pub struct JsonDialog(ObjectSubclass<imp::JsonDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl JsonDialog {
    pub fn new(manager: &ConnectionManager) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.manager.set(manager.clone()).expect("set once");
        let profiles = manager.profiles();
        let has_override = profiles.config_override.is_some();
        let text = profiles
            .config_override
            .or_else(|| manager.generated_config_json())
            .unwrap_or_else(|| gettext("Select a server first."));
        imp.text_view.buffer().set_text(&text);
        imp.reset_button.set_sensitive(has_override);
        dialog
    }

    fn text(&self) -> String {
        let b = self.imp().text_view.buffer();
        b.text(&b.start_iter(), &b.end_iter(), false).to_string()
    }

    fn show_result(&self, text: &str, is_error: bool) {
        let label = &self.imp().result_label;
        label.set_label(text);
        label.remove_css_class("error");
        label.remove_css_class("success");
        label.add_css_class(if is_error { "error" } else { "success" });
        label.set_visible(true);
    }

    /// Runs `xray run -test` on the text; on success and `apply`, stores it.
    fn validate(&self, apply: bool) {
        let text = self.text();
        let value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.show_result(
                    &gettext("Not valid JSON: %s").replace("%s", &e.to_string()),
                    true,
                );
                return;
            }
        };
        let imp = self.imp();
        imp.validate_button.set_sensitive(false);
        imp.apply_button.set_sensitive(false);
        let core_dir = paths::core_dir();
        let task = crate::runtime::runtime().spawn(async move {
            let path = paths::cache_dir().join("config-check.json");
            write_config(&path, &value)?;
            let sup = Supervisor::new(core_dir.join("xray"), core_dir);
            let r = sup.test_config(&path).await;
            let _ = std::fs::remove_file(&path);
            r
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                let imp = dialog.imp();
                imp.validate_button.set_sensitive(true);
                imp.apply_button.set_sensitive(true);
                match task.await {
                    Ok(Ok(())) => {
                        if apply {
                            if let Some(m) = imp.manager.get() {
                                m.set_config_override(Some(text));
                            }
                            imp.reset_button.set_sensitive(true);
                            dialog.show_result(
                                &gettext("Configuration accepted by the engine and in use."),
                                false,
                            );
                        } else {
                            dialog.show_result(
                                &gettext("Configuration accepted by the engine."),
                                false,
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        // Tunnel configs fail -test without capabilities; say so.
                        let detail = e.to_string();
                        let msg = if detail.contains("operation not permitted") {
                            gettext(
                                "The engine accepted the configuration but cannot open the tunnel without permission. Connect once in Tunnel mode to grant it.",
                            )
                        } else {
                            gettext("The engine rejected it: %s").replace("%s", &detail)
                        };
                        dialog.show_result(&msg, !detail.contains("operation not permitted"));
                    }
                    Err(_) => {}
                }
            }
        ));
    }
}
