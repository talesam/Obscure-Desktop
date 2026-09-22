//! "Live connections": a terminal-like stream of what is going where,
//! built from Xray's access log. Colour tells the route: via the server,
//! direct, or blocked.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use gtk::glib::clone;
use obscure_core::access::{AccessEvent, Verdict};

use crate::connection::{AccessEventBox, ConnectionManager};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/connections-dialog.ui")]
    pub struct ConnectionsDialog {
        #[template_child]
        pub pause_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub counter_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub scrolled: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub handlers: std::cell::RefCell<Vec<glib::SignalHandlerId>>,
        pub count: std::cell::Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConnectionsDialog {
        const NAME: &'static str = "ObscureConnectionsDialog";
        type Type = super::ConnectionsDialog;
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
    impl ConnectionsDialog {
        #[template_callback]
        fn on_copy_clicked(&self, _b: &gtk::Button) {
            let buffer = self.text_view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            self.obj().clipboard().set_text(&text);
        }

        #[template_callback]
        fn on_clear_clicked(&self, _b: &gtk::Button) {
            self.text_view.buffer().set_text("");
            self.count.set(0);
            self.obj().update_counter();
        }
    }

    impl ObjectImpl for ConnectionsDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let buffer = self.text_view.buffer();
            for (name, class) in [
                ("proxy", "accent"),
                ("direct", "dim"),
                ("block", "error"),
                ("time", "time"),
                ("rejected", "warning"),
            ] {
                let tag = gtk::TextTag::new(Some(name));
                match class {
                    "accent" => tag.set_foreground(Some("#3584e4")),
                    "dim" => tag.set_foreground(Some("#9a9996")),
                    "error" => {
                        tag.set_foreground(Some("#e01b24"));
                        tag.set_strikethrough(true);
                    }
                    "warning" => tag.set_foreground(Some("#e5a50a")),
                    _ => tag.set_foreground(Some("#77767b")),
                }
                buffer.tag_table().add(&tag);
            }
        }

        fn dispose(&self) {
            if let Some(m) = self.manager.get() {
                for id in self.handlers.take() {
                    glib::signal::signal_handler_disconnect(m, id);
                }
            }
        }
    }
    impl WidgetImpl for ConnectionsDialog {}
    impl AdwDialogImpl for ConnectionsDialog {}
}

glib::wrapper! {
    pub struct ConnectionsDialog(ObjectSubclass<imp::ConnectionsDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl ConnectionsDialog {
    pub fn new(manager: &ConnectionManager) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.manager.set(manager.clone()).expect("set once");

        for ev in manager.access_events() {
            dialog.append(&ev);
        }
        dialog.update_counter();

        manager
            .bind_property("connected", &*imp.banner, "revealed")
            .invert_boolean()
            .sync_create()
            .build();

        let id = manager.connect_closure(
            "access-event",
            false,
            glib::closure_local!(
                #[weak]
                dialog,
                move |_m: ConnectionManager, ev: AccessEventBox| {
                    if !dialog.imp().pause_button.is_active() {
                        dialog.append(&ev.0);
                        dialog.update_counter();
                    }
                }
            ),
        );
        imp.handlers.borrow_mut().push(id);
        dialog
    }

    fn append(&self, ev: &AccessEvent) {
        let imp = self.imp();
        let buffer = imp.text_view.buffer();
        let mut end = buffer.end_iter();
        if buffer.char_count() > 0 {
            buffer.insert(&mut end, "\n");
        }
        buffer.insert_with_tags_by_name(&mut end, &format!("{}  ", ev.time), &["time"]);

        let (arrow, tag, route) = match (ev.verdict, ev.outbound.as_str()) {
            (Verdict::Rejected, _) => ("✕", "rejected", gettext("rejected")),
            (_, "block") => ("✕", "block", gettext("blocked")),
            (_, "direct") => ("→", "direct", gettext("direct")),
            (_, "proxy") => ("→", "proxy", gettext("via server")),
            // DNS-over-HTTPS lookups are logged as `[local]`.
            (_, "local") => ("→", "direct", gettext("DNS lookup")),
            (_, other) => ("→", "direct", other.to_owned()),
        };
        let net = ev.network.as_deref().unwrap_or("").to_uppercase();
        let method = ev
            .method
            .as_deref()
            .map(|m| format!("{m} "))
            .unwrap_or_default();
        let line = if net.is_empty() {
            format!("{method}{}  {arrow} {route}", ev.destination)
        } else {
            format!("{net:<3} {method}{}  {arrow} {route}", ev.destination)
        };
        buffer.insert_with_tags_by_name(&mut end, &line, &[tag]);

        imp.count.set(imp.count.get() + 1);
        let scrolled = imp.scrolled.clone();
        glib::idle_add_local_once(clone!(
            #[weak]
            scrolled,
            move || {
                let adj = scrolled.vadjustment();
                adj.set_value(adj.upper() - adj.page_size());
            }
        ));
    }

    fn update_counter(&self) {
        let n = self.imp().count.get();
        self.imp().counter_label.set_label(&n.to_string());
    }
}
