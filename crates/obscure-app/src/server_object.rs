//! GObject wrapper around a stored server, for list models.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use obscure_core::profile::ServerEntry;

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::ServerObject)]
    pub struct ServerObject {
        #[property(get, set)]
        pub id: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub subtitle: RefCell<String>,
        #[property(get, set)]
        pub selected: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ServerObject {
        const NAME: &'static str = "ObscureServerObject";
        type Type = super::ServerObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ServerObject {}
}

glib::wrapper! {
    pub struct ServerObject(ObjectSubclass<imp::ServerObject>);
}

impl ServerObject {
    pub fn from_entry(entry: &ServerEntry, selected: bool) -> Self {
        let s = &entry.server;
        let subtitle = format!(
            "{} · {} · {}:{}",
            s.protocol.label(),
            s.security.label(),
            s.address,
            s.port
        );
        glib::Object::builder()
            .property("id", &entry.id)
            .property("name", &s.name)
            .property("subtitle", subtitle)
            .property("selected", selected)
            .build()
    }
}
