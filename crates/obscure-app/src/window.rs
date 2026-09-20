use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::config::{APP_ID, PROFILE};

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
        pub status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub add_server_button: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ObscureWindow {
        const NAME: &'static str = "ObscureWindow";
        type Type = super::ObscureWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl ObscureWindow {
        #[template_callback]
        fn on_add_server_clicked(&self, _button: &gtk::Button) {
            // Phase 2 opens the import dialog here. The Connect button only
            // appears once at least one server exists.
            tracing::info!("add server requested (not implemented yet)");
            self.toast_overlay.add_toast(adw::Toast::new(&gettext(
                "Importar servidores chega na próxima versão.",
            )));
        }
    }

    impl ObjectImpl for ObscureWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            if PROFILE == "development" {
                obj.add_css_class("devel");
            }
            self.status_page
                .set_icon_name(Some(&format!("{APP_ID}-symbolic")));
            obj.bind_settings();
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
    pub fn new<P: IsA<gtk::Application>>(application: &P) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    /// Persists window geometry in the gschema (`window-width`,
    /// `window-height`, `window-maximized`).
    fn bind_settings(&self) {
        let settings = gio::Settings::new(APP_ID);
        settings.bind("window-width", self, "default-width").build();
        settings
            .bind("window-height", self, "default-height")
            .build();
        settings.bind("window-maximized", self, "maximized").build();
    }
}
