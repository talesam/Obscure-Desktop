//! Dialog to add servers by pasting share links.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::glib;
use gtk::glib::clone;
use obscure_core::links::{Server, parse_many};
use obscure_core::profile::looks_like_subscription_url;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/import-dialog.ui")]
    pub struct ImportDialog {
        #[template_child]
        pub import_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub paste_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub hint_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub qr_image_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub qr_screen_button: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImportDialog {
        const NAME: &'static str = "ObscureImportDialog";
        type Type = super::ImportDialog;
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
    impl ImportDialog {
        #[template_callback]
        fn on_import_clicked(&self, _button: &gtk::Button) {
            let obj = self.obj();
            let text = obj.text();
            if let Some(url) = subscription_url_in(&text) {
                obj.emit_by_name::<()>("subscription-url", &[&url]);
                obj.close();
                return;
            }
            let (servers, failed) = parse_many(&text);
            if servers.is_empty() {
                obj.show_hint(&gettext("No valid link found."));
                return;
            }
            if !failed.is_empty() {
                tracing::warn!("{} link(s) ignored while importing", failed.len());
            }
            obj.emit_by_name::<()>("servers-parsed", &[&ServersBox(servers)]);
            obj.close();
        }

        #[template_callback]
        fn on_qr_image_clicked(&self, _button: &gtk::Button) {
            let obj = self.obj();
            let filter = gtk::FileFilter::new();
            filter.set_name(Some(&gettext("Images")));
            filter.add_mime_type("image/*");
            let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let dialog = gtk::FileDialog::builder()
                .title(gettext("Choose an image with a QR code"))
                .modal(true)
                .filters(&filters)
                .build();
            let parent = obj.root().and_downcast::<gtk::Window>();
            dialog.open(
                parent.as_ref(),
                gtk::gio::Cancellable::NONE,
                clone!(
                    #[weak]
                    obj,
                    move |result| {
                        let Ok(file) = result else { return };
                        let Some(path) = file.path() else { return };
                        match crate::qr::decode_image_file(&path) {
                            Ok(text) => obj.set_text(&text),
                            Err(e) => {
                                tracing::warn!("QR decode failed: {e}");
                                obj.show_hint(&gettext("No QR code was found in that image."));
                            }
                        }
                    }
                ),
            );
        }

        #[template_callback]
        fn on_qr_screen_clicked(&self, _button: &gtk::Button) {
            let obj = self.obj();
            self.qr_screen_button.set_sensitive(false);
            let task = crate::runtime::runtime().spawn(crate::qr::decode_from_screen());
            glib::spawn_future_local(clone!(
                #[weak]
                obj,
                async move {
                    match task.await {
                        Ok(Ok(text)) => obj.set_text(&text),
                        Ok(Err(e)) => {
                            tracing::warn!("screen QR failed: {e}");
                            obj.show_hint(&gettext(
                                "No QR code was found on the screen. Make sure it is fully visible and try again.",
                            ));
                        }
                        Err(_) => {}
                    }
                    obj.imp().qr_screen_button.set_sensitive(true);
                }
            ));
        }

        #[template_callback]
        fn on_paste_clicked(&self, _button: &gtk::Button) {
            let obj = self.obj();
            let clipboard = obj.clipboard();
            glib::spawn_future_local(clone!(
                #[weak]
                obj,
                async move {
                    if let Ok(Some(text)) = clipboard.read_text_future().await {
                        obj.set_text(&text);
                        return;
                    }
                    // An image on the clipboard: try to read a QR code from it.
                    if let Ok(texture) = clipboard.read_texture_future().await
                        && let Some(texture) = texture
                    {
                        let png = texture.save_to_png_bytes();
                        match crate::qr::decode_image_bytes(&png) {
                            Ok(text) => obj.set_text(&text),
                            Err(_) => {
                                obj.show_hint(&gettext("No QR code was found in that image."))
                            }
                        }
                    }
                }
            ));
        }
    }

    impl ObjectImpl for ImportDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.text_view.buffer().connect_changed(clone!(
                #[weak]
                obj,
                move |_| obj.refresh_preview()
            ));
        }

        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::OnceLock;
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("servers-parsed")
                        .param_types([ServersBox::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("subscription-url")
                        .param_types([String::static_type()])
                        .build(),
                ]
            })
        }
    }

    impl WidgetImpl for ImportDialog {}
    impl AdwDialogImpl for ImportDialog {}
}

glib::wrapper! {
    pub struct ImportDialog(ObjectSubclass<imp::ImportDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

/// Boxed list of parsed servers, so it can travel through a GLib signal.
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "ObscureServersBox")]
pub struct ServersBox(pub Vec<Server>);

impl Default for ImportDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Pre-fills the text area (e.g. from the clipboard or a URL handler).
    pub fn with_text(text: &str) -> Self {
        let d = Self::new();
        d.set_text(text);
        d
    }

    fn text(&self) -> String {
        let buffer = self.imp().text_view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn set_text(&self, text: &str) {
        self.imp().text_view.buffer().set_text(text);
    }

    fn show_hint(&self, text: &str) {
        let hint = &self.imp().hint_label;
        hint.set_label(text);
        hint.set_visible(true);
    }

    fn refresh_preview(&self) {
        let text = self.text();
        let imp = self.imp();
        if text.trim().is_empty() {
            imp.import_button.set_sensitive(false);
            imp.hint_label.set_visible(false);
            return;
        }
        if subscription_url_in(&text).is_some() {
            imp.import_button.set_sensitive(true);
            self.show_hint(&gettext(
                "Subscription address: the servers will be downloaded and kept up to date.",
            ));
            return;
        }
        let (servers, failed) = parse_many(&text);
        imp.import_button.set_sensitive(!servers.is_empty());
        let mut msg = if servers.is_empty() {
            gettext("No valid link found.")
        } else {
            ngettext(
                "%n server ready to add.",
                "%n servers ready to add.",
                servers.len() as u32,
            )
            .replace("%n", &servers.len().to_string())
        };
        if !failed.is_empty() {
            msg.push(' ');
            msg.push_str(
                &ngettext(
                    "%n link could not be read.",
                    "%n links could not be read.",
                    failed.len() as u32,
                )
                .replace("%n", &failed.len().to_string()),
            );
        }
        self.show_hint(&msg);
    }
}

/// If the text is a single `http(s)://` address, returns it.
fn subscription_url_in(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    if lines.next().is_none() && looks_like_subscription_url(first) {
        Some(first.to_owned())
    } else {
        None
    }
}
