//! GObject wrapper around a stored server, for list models.

use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use obscure_core::latency::{LatencyClass, classify};
use obscure_core::profile::ServerEntry;

/// Sentinel values for `latency_ms`.
pub const LATENCY_UNKNOWN: i32 = -1;
pub const LATENCY_FAILED: i32 = -2;
pub const LATENCY_TESTING: i32 = -3;

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
        #[property(get, set)]
        pub group: RefCell<String>,
        /// Milliseconds, or one of the LATENCY_* sentinels.
        #[property(get, set, default = -1)]
        pub latency_ms: Cell<i32>,
        /// ISO alpha-2 country code ("" when unknown).
        #[property(get, set)]
        pub country: RefCell<String>,
        #[property(get, set)]
        pub address: RefCell<String>,
        #[property(get, set)]
        pub port: Cell<u32>,
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
    pub fn from_entry(entry: &ServerEntry, selected: bool, group_name: Option<&str>) -> Self {
        let s = &entry.server;
        let mut subtitle = format!(
            "{} · {} · {}:{}",
            s.protocol.label(),
            s.security.label(),
            s.address,
            s.port
        );
        if let Some(g) = group_name {
            subtitle = format!("{g} · {subtitle}");
        }
        glib::Object::builder()
            .property("id", &entry.id)
            .property("name", &s.name)
            .property("subtitle", subtitle)
            .property("selected", selected)
            .property("group", entry.group.clone().unwrap_or_default())
            .property("latency-ms", LATENCY_UNKNOWN)
            .property("country", entry.country.clone().unwrap_or_default())
            .property("address", &s.address)
            .property("port", u32::from(s.port))
            .build()
    }

    /// Flag emoji for the country, or `None` when unknown.
    pub fn flag(&self) -> Option<String> {
        obscure_core::geoip::flag_emoji(&self.country())
    }

    /// Text and CSS class for the latency badge.
    pub fn latency_badge(ms: i32) -> Option<(String, &'static str)> {
        match ms {
            LATENCY_UNKNOWN => None,
            LATENCY_TESTING => Some(("…".into(), "dim-label")),
            LATENCY_FAILED => Some(("✕".into(), "error")),
            ms => {
                let class = match classify(ms as u32) {
                    LatencyClass::Good => "success",
                    LatencyClass::Fair => "warning",
                    LatencyClass::Poor => "error",
                };
                Some((format!("{ms} ms"), class))
            }
        }
    }
}
