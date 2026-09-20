//! Server details: QR code, protocol chips, rename, copy link, remove.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use obscure_core::links::{Server, to_link};

use crate::connection::ConnectionManager;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/server-dialog.ui")]
    pub struct ServerDialog {
        #[template_child]
        pub qr_picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub chips: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub name_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub address_row: TemplateChild<adw::ActionRow>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub id: std::cell::RefCell<String>,
        pub server: std::cell::RefCell<Option<Server>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ServerDialog {
        const NAME: &'static str = "ObscureServerDialog";
        type Type = super::ServerDialog;
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
    impl ServerDialog {
        #[template_callback]
        fn on_name_applied(&self, row: &adw::EntryRow) {
            let Some(m) = self.manager.get() else { return };
            if m.rename_server(&self.id.borrow(), &row.text()) {
                self.obj().set_title(&row.text());
            }
        }

        #[template_callback]
        fn on_copy_link_clicked(&self, _b: &gtk::Button) {
            if let Some(server) = self.server.borrow().as_ref() {
                self.obj().clipboard().set_text(&to_link(server));
                self.obj()
                    .emit_by_name::<()>("toast", &[&gettext("Link copied.")]);
            }
        }

        #[template_callback]
        fn on_remove_clicked(&self, _b: &gtk::Button) {
            let obj = self.obj();
            obj.emit_by_name::<()>("remove-requested", &[&*self.id.borrow()]);
            obj.close();
        }
    }

    impl ObjectImpl for ServerDialog {
        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::OnceLock;
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("remove-requested")
                        .param_types([String::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("toast")
                        .param_types([String::static_type()])
                        .build(),
                ]
            })
        }
    }
    impl WidgetImpl for ServerDialog {}
    impl AdwDialogImpl for ServerDialog {}
}

glib::wrapper! {
    pub struct ServerDialog(ObjectSubclass<imp::ServerDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl ServerDialog {
    pub fn new(manager: &ConnectionManager, id: &str) -> Option<Self> {
        let entry = manager.server_entry(id)?;
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.manager.set(manager.clone()).expect("set once");
        *imp.id.borrow_mut() = id.to_owned();
        let server = entry.server;

        dialog.set_title(&server.name);
        imp.name_row.set_text(&server.name);
        imp.address_row
            .set_subtitle(&format!("{}:{}", server.address, server.port));

        for label in [
            server.protocol.label().to_owned(),
            server.transport.label().to_owned(),
            server.security.label().to_owned(),
        ] {
            let chip = gtk::Label::builder()
                .label(&label)
                .css_classes(["obscure-chip"])
                .build();
            imp.chips.append(&chip);
        }

        match qr_texture(&to_link(&server)) {
            Ok(texture) => imp.qr_picture.set_paintable(Some(&texture)),
            Err(e) => {
                tracing::warn!("cannot render QR: {e}");
                imp.qr_picture.set_visible(false);
            }
        }
        *imp.server.borrow_mut() = Some(server);
        Some(dialog)
    }
}

/// Renders `text` as a QR code into a GDK texture (PNG in memory).
fn qr_texture(text: &str) -> Result<gtk::gdk::Texture, Box<dyn std::error::Error>> {
    let code = qrcode::QrCode::new(text.as_bytes())?;
    let image = code
        .render::<image::Luma<u8>>()
        .quiet_zone(true)
        .min_dimensions(200, 200)
        .max_dimensions(200, 200)
        .build();
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    let bytes = glib::Bytes::from_owned(png);
    Ok(gtk::gdk::Texture::from_bytes(&bytes)?)
}
