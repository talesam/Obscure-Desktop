//! GObject wrapper around a subscription, for list models.

use std::cell::{Cell, RefCell};

use gettextrs::{gettext, ngettext};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use obscure_core::profile::Subscription;
use obscure_core::stats::format_bytes;

mod imp {
    use super::*;

    #[derive(Debug, Default, glib::Properties)]
    #[properties(wrapper_type = super::SubscriptionObject)]
    pub struct SubscriptionObject {
        #[property(get, set)]
        pub id: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        #[property(get, set)]
        pub subtitle: RefCell<String>,
        /// 0.0–1.0 of the quota used; `has_quota` gates the bar.
        #[property(get, set)]
        pub fraction: Cell<f64>,
        #[property(get, set)]
        pub has_quota: Cell<bool>,
        #[property(get, set)]
        pub updating: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SubscriptionObject {
        const NAME: &'static str = "ObscureSubscriptionObject";
        type Type = super::SubscriptionObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for SubscriptionObject {}
}

glib::wrapper! {
    pub struct SubscriptionObject(ObjectSubclass<imp::SubscriptionObject>);
}

impl SubscriptionObject {
    pub fn from_subscription(sub: &Subscription, server_count: usize) -> Self {
        let mut parts = vec![
            ngettext("%n server", "%n servers", server_count as u32)
                .replace("%n", &server_count.to_string()),
        ];
        let mut fraction = 0.0;
        let mut has_quota = false;
        if let Some(info) = &sub.user_info {
            if let (Some(used), Some(total)) = (info.used(), info.total) {
                parts.push(
                    gettext("%a of %b used")
                        .replace("%a", &format_bytes(used))
                        .replace("%b", &format_bytes(total)),
                );
                fraction = info.fraction_used().unwrap_or(0.0);
                has_quota = true;
            }
            if let Some(expire) = info.expire.filter(|e| *e > 0) {
                let dt = glib::DateTime::from_unix_local(expire as i64)
                    .ok()
                    .and_then(|d| d.format("%x").ok())
                    .map(|g| g.to_string())
                    .unwrap_or_default();
                parts.push(gettext("expires %d").replace("%d", &dt));
            }
        }
        glib::Object::builder()
            .property("id", &sub.id)
            .property("name", &sub.name)
            .property("subtitle", parts.join(" · "))
            .property("fraction", fraction)
            .property("has-quota", has_quota)
            .build()
    }
}
