//! Raw Xray log, hidden behind a menu entry (see PLANO §1: errors are
//! humanised elsewhere; this is for the curious and for bug reports).

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use gtk::glib::clone;

use crate::connection::ConnectionManager;

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/talesam/Obscure/ui/log-dialog.ui")]
    pub struct LogDialog {
        #[template_child]
        pub scrolled: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub text_view: TemplateChild<gtk::TextView>,
        pub manager: std::cell::OnceCell<ConnectionManager>,
        pub handler: std::cell::RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LogDialog {
        const NAME: &'static str = "ObscureLogDialog";
        type Type = super::LogDialog;
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
    impl LogDialog {
        #[template_callback]
        fn on_copy_clicked(&self, _b: &gtk::Button) {
            let buffer = self.text_view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            self.obj().clipboard().set_text(&text);
        }

        #[template_callback]
        fn on_clear_clicked(&self, _b: &gtk::Button) {
            if let Some(m) = self.manager.get() {
                m.clear_log();
            }
            self.text_view.buffer().set_text("");
        }
    }

    impl ObjectImpl for LogDialog {
        fn dispose(&self) {
            if let (Some(m), Some(id)) = (self.manager.get(), self.handler.take()) {
                glib::signal::signal_handler_disconnect(m, id);
            }
        }
    }
    impl WidgetImpl for LogDialog {}
    impl AdwDialogImpl for LogDialog {}
}

glib::wrapper! {
    pub struct LogDialog(ObjectSubclass<imp::LogDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl LogDialog {
    pub fn new(manager: &ConnectionManager) -> Self {
        let dialog: Self = glib::Object::new();
        let imp = dialog.imp();
        imp.manager.set(manager.clone()).expect("set once");

        let lines = manager.log_lines();
        let text = if lines.is_empty() {
            gettext("Nothing logged yet. Connect to see what the engine reports.")
        } else {
            lines.join("\n")
        };
        imp.text_view.buffer().set_text(&text);
        dialog.scroll_to_end();

        let id = manager.connect_closure(
            "log-line",
            false,
            glib::closure_local!(
                #[weak]
                dialog,
                move |_m: ConnectionManager, line: String| {
                    let buffer = dialog.imp().text_view.buffer();
                    let mut end = buffer.end_iter();
                    if buffer.char_count() > 0 {
                        buffer.insert(&mut end, "\n");
                    }
                    buffer.insert(&mut end, &line);
                    dialog.scroll_to_end();
                }
            ),
        );
        *imp.handler.borrow_mut() = Some(id);
        dialog
    }

    fn scroll_to_end(&self) {
        let scrolled = self.imp().scrolled.clone();
        glib::idle_add_local_once(clone!(
            #[weak]
            scrolled,
            move || {
                let adj = scrolled.vadjustment();
                adj.set_value(adj.upper() - adj.page_size());
            }
        ));
    }
}
