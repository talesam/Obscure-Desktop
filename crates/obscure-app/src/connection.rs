//! Connection state machine exposed to the UI as a GObject, plus the tokio
//! actor that owns the Xray process while connected.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use gettextrs::gettext;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib::clone};
use obscure_core::access::{AccessEvent, parse_access_line};
use obscure_core::config::{ConfigOptions, build};
use obscure_core::core_manager::{CoreManager, Progress};
use obscure_core::links::Server;
use obscure_core::profile::{ApplyMode, Profiles, RoutePreset, RouteRule};
use obscure_core::stats::{RateMeter, StatsClient, TrafficSample, format_bytes, format_rate};
use obscure_core::supervisor::{CoreEvent, RestartPolicy, RunningCore, Supervisor, write_config};
use obscure_core::sysproxy::SysProxy;
use obscure_core::tun::{CapStatus, capabilities, grant};
use obscure_core::{Error, paths};

use crate::humanize::error_message;
use crate::runtime::runtime;
use crate::server_object::{LATENCY_FAILED, LATENCY_TESTING, ServerObject};
use crate::subscription_object::SubscriptionObject;

const MAX_LOG_LINES: usize = 500;
const MAX_ACCESS_EVENTS: usize = 1000;

/// Boxed access event so it can travel through a GLib signal.
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "ObscureAccessEventBox")]
pub struct AccessEventBox(pub AccessEvent);

/// Messages from the tokio side to the UI.
#[derive(Debug)]
enum UiEvent {
    Progress(Progress),
    Log(String),
    Connected,
    /// The core died and is being restarted (attempt number).
    Restarting(u32),
    Traffic(TrafficSample),
    Disconnected,
    Failed(Error),
}

/// Everything the actor needs to (re)build the Xray configuration.
#[derive(Debug, Clone)]
struct SessionParams {
    server: Server,
    local_port: u16,
    route_preset: RoutePreset,
    listed_domains: Vec<String>,
    apply_mode: ApplyMode,
    dns: String,
    prerelease: bool,
    custom_rules: Vec<RouteRule>,
    config_override: Option<String>,
}

/// Commands from the UI to the actor.
#[derive(Debug)]
pub enum Cmd {
    Stop,
}

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::ConnectionManager)]
    pub struct ConnectionManager {
        /// Connected and usable.
        #[property(get, set)]
        pub connected: Cell<bool>,
        /// Connecting, disconnecting or downloading the core.
        #[property(get, set)]
        pub busy: Cell<bool>,
        /// One-line human status shown under the server name.
        #[property(get, set)]
        pub status_text: RefCell<String>,
        /// 0.0–1.0 download progress; `progress_visible` gates the bar.
        #[property(get, set)]
        pub progress: Cell<f64>,
        #[property(get, set)]
        pub progress_visible: Cell<bool>,
        /// Name of the selected server ("" when none).
        #[property(get, set)]
        pub selected_name: RefCell<String>,
        #[property(get, set)]
        pub has_servers: Cell<bool>,
        /// `local_only`, `system_proxy` or `tunnel` (mirrors ApplyMode).
        #[property(get, set)]
        pub apply_mode: RefCell<String>,
        /// "↑ 1.2 kB/s · ↓ 3.4 MB/s · 12 MB" while connected, else "".
        #[property(get, set)]
        pub traffic_text: RefCell<String>,
        /// `all`, `smart` or `only_listed` (mirrors RoutePreset).
        #[property(get, set)]
        pub route_preset: RefCell<String>,
        /// Connect to the fastest server instead of the selected one.
        #[property(get, set)]
        pub auto_select: Cell<bool>,
        /// A latency test is running.
        #[property(get, set)]
        pub testing: Cell<bool>,
        #[property(get, set)]
        pub has_subscriptions: Cell<bool>,
        /// Flag emoji of the selected server ("" when unknown).
        #[property(get, set)]
        pub selected_flag: RefCell<String>,

        pub profiles: RefCell<Profiles>,
        pub servers: gio::ListStore,
        pub subscriptions: gio::ListStore,
        /// Last latency result per server id (survives list rebuilds).
        pub latency: RefCell<HashMap<String, i32>>,
        pub log: RefCell<VecDeque<String>>,
        pub access: RefCell<VecDeque<AccessEvent>>,
        pub cmd_tx: RefCell<Option<tokio::sync::mpsc::Sender<Cmd>>>,
        pub last_error: RefCell<Option<String>>,
    }

    impl Default for ConnectionManager {
        fn default() -> Self {
            Self {
                connected: Cell::new(false),
                busy: Cell::new(false),
                status_text: RefCell::new(String::new()),
                progress: Cell::new(0.0),
                progress_visible: Cell::new(false),
                selected_name: RefCell::new(String::new()),
                has_servers: Cell::new(false),
                apply_mode: RefCell::new("system_proxy".into()),
                traffic_text: RefCell::new(String::new()),
                route_preset: RefCell::new("smart".into()),
                auto_select: Cell::new(false),
                testing: Cell::new(false),
                has_subscriptions: Cell::new(false),
                selected_flag: RefCell::new(String::new()),
                profiles: RefCell::new(Profiles::default()),
                servers: gio::ListStore::new::<ServerObject>(),
                subscriptions: gio::ListStore::new::<SubscriptionObject>(),
                latency: RefCell::new(HashMap::new()),
                log: RefCell::new(VecDeque::new()),
                access: RefCell::new(VecDeque::new()),
                cmd_tx: RefCell::new(None),
                last_error: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConnectionManager {
        const NAME: &'static str = "ObscureConnectionManager";
        type Type = super::ConnectionManager;
    }

    #[glib::derived_properties]
    impl ObjectImpl for ConnectionManager {
        fn signals() -> &'static [glib::subclass::Signal] {
            use std::sync::OnceLock;
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("failed").build(),
                    // Emitted for every new log line (String).
                    glib::subclass::Signal::builder("log-line")
                        .param_types([String::static_type()])
                        .build(),
                    // One parsed access-log record.
                    glib::subclass::Signal::builder("access-event")
                        .param_types([AccessEventBox::static_type()])
                        .build(),
                    // Something worth a desktop notification: (title, body, important).
                    glib::subclass::Signal::builder("notification")
                        .param_types([
                            String::static_type(),
                            String::static_type(),
                            bool::static_type(),
                        ])
                        .build(),
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct ConnectionManager(ObjectSubclass<imp::ConnectionManager>);
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        let obj: Self = glib::Object::new();
        obj.load_profiles();
        obj.set_status_text(gettext("Disconnected"));
        obj
    }

    // -- profiles -----------------------------------------------------------

    fn load_profiles(&self) {
        let profiles = match Profiles::load(&paths::profiles_file()) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("cannot load profiles: {e}");
                Profiles::default()
            }
        };
        *self.imp().profiles.borrow_mut() = profiles;
        self.sync_from_profiles();
        self.resolve_countries();
    }

    fn save_profiles(&self) {
        if let Err(e) = self.imp().profiles.borrow().save(&paths::profiles_file()) {
            tracing::error!("cannot save profiles: {e}");
        }
    }

    /// Rebuilds the list models and derived properties from `profiles`.
    fn sync_from_profiles(&self) {
        let imp = self.imp();
        let profiles = imp.profiles.borrow();
        let latency = imp.latency.borrow();
        let group_name = |id: &Option<String>| -> Option<String> {
            id.as_deref()
                .and_then(|g| profiles.subscription(g))
                .map(|s| s.name.clone())
        };
        let objects: Vec<ServerObject> = profiles
            .servers
            .iter()
            .map(|e| {
                let obj = ServerObject::from_entry(
                    e,
                    profiles.selected.as_deref() == Some(&e.id),
                    group_name(&e.group).as_deref(),
                );
                if let Some(ms) = latency.get(&e.id) {
                    obj.set_latency_ms(*ms);
                }
                obj
            })
            .collect();
        imp.servers.remove_all();
        imp.servers.extend_from_slice(&objects);

        let subs: Vec<SubscriptionObject> = profiles
            .subscriptions
            .iter()
            .map(|s| SubscriptionObject::from_subscription(s, profiles.servers_in_group(&s.id)))
            .collect();
        imp.subscriptions.remove_all();
        imp.subscriptions.extend_from_slice(&subs);
        self.set_has_subscriptions(!subs.is_empty());

        self.set_has_servers(!profiles.servers.is_empty());
        self.set_selected_name(
            profiles
                .selected_entry()
                .map(|e| e.server.name.clone())
                .unwrap_or_default(),
        );
        self.set_selected_flag(
            profiles
                .selected_entry()
                .and_then(|e| e.country.as_deref())
                .and_then(obscure_core::geoip::flag_emoji)
                .unwrap_or_default(),
        );
        self.set_apply_mode(mode_to_str(profiles.apply_mode));
        self.set_route_preset(preset_to_str(profiles.route_preset));
        self.set_auto_select(profiles.auto_select);
    }

    pub fn subscriptions(&self) -> &gio::ListStore {
        &self.imp().subscriptions
    }

    /// Read-only access to the stored profiles (settings, servers).
    pub fn profiles(&self) -> Profiles {
        self.imp().profiles.borrow().clone()
    }

    pub fn server_entry(&self, id: &str) -> Option<obscure_core::profile::ServerEntry> {
        self.imp().profiles.borrow().get(id).cloned()
    }

    pub fn rename_server(&self, id: &str, name: &str) -> bool {
        let ok = self.imp().profiles.borrow_mut().rename(id, name);
        if ok {
            self.save_profiles();
            self.sync_from_profiles();
        }
        ok
    }

    pub fn set_route_preset_from_ui(&self, preset: &str) {
        let Some(preset) = str_to_preset(preset) else {
            return;
        };
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.route_preset != preset;
            p.route_preset = preset;
            changed
        };
        if changed {
            self.save_profiles();
            self.set_route_preset(preset_to_str(preset));
            if self.connected() {
                self.reconnect();
            }
        }
    }

    /// The configuration Obscure would generate right now (for the JSON editor).
    pub fn generated_config_json(&self) -> Option<String> {
        let p = self.imp().profiles.borrow();
        let entry = p.selected_entry()?;
        let mut opts = ConfigOptions::new(&entry.server);
        opts.local_port = p.local_port;
        opts.route_preset = p.route_preset;
        opts.listed_domains = &p.listed_domains;
        opts.dns = &p.dns;
        opts.tunnel = p.apply_mode == ApplyMode::Tunnel;
        opts.custom_rules = &p.custom_rules;
        serde_json::to_string_pretty(&build(&opts)).ok()
    }

    pub fn set_custom_rules(&self, rules: Vec<RouteRule>) {
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.custom_rules != rules;
            p.custom_rules = rules;
            changed
        };
        if changed {
            self.save_profiles();
            if self.connected() {
                self.reconnect();
            }
        }
    }

    pub fn set_config_override(&self, json: Option<String>) {
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.config_override != json;
            p.config_override = json;
            changed
        };
        if changed {
            self.save_profiles();
            if self.connected() {
                self.reconnect();
            }
        }
    }

    /// Updates connection settings from Preferences. Reconnects if needed.
    pub fn update_settings(&self, local_port: u16, dns: &str, prerelease: bool) {
        let dns = if dns.trim().is_empty() {
            obscure_core::profile::DEFAULT_DNS.to_owned()
        } else {
            dns.trim().to_owned()
        };
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.local_port != local_port || p.dns != dns || p.prerelease != prerelease;
            p.local_port = local_port;
            p.dns = dns;
            p.prerelease = prerelease;
            changed
        };
        if changed {
            self.save_profiles();
            if self.connected() {
                self.reconnect();
            }
        }
    }

    // -- subscriptions -------------------------------------------------------

    fn http_client() -> obscure_core::HttpClient {
        obscure_core::http_client(&obscure_core::user_agent(obscure_core::APP_VERSION))
    }

    /// Fetches `url` and adds it as a subscription. `done` receives the
    /// number of servers or the error.
    pub fn add_subscription_url(
        &self,
        url: String,
        done: impl FnOnce(Result<usize, Error>) + 'static,
    ) {
        let task = runtime().spawn(async move {
            let client = Self::http_client();
            obscure_core::subscription::fetch(&client, &url)
                .await
                .map(|f| (url, f))
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = match task.await {
                    Ok(Ok((url, fetched))) => {
                        let count = fetched.servers.len();
                        this.imp()
                            .profiles
                            .borrow_mut()
                            .add_subscription(&url, &fetched);
                        this.save_profiles();
                        this.sync_from_profiles();
                        Ok(count)
                    }
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(Error::Subscription("task cancelled".into())),
                };
                done(result);
            }
        ));
    }

    /// Re-downloads one subscription.
    pub fn update_subscription(
        &self,
        id: String,
        done: impl FnOnce(Result<usize, Error>) + 'static,
    ) {
        let Some(url) = self
            .imp()
            .profiles
            .borrow()
            .subscription(&id)
            .map(|s| s.url.clone())
        else {
            return;
        };
        self.set_subscription_updating(&id, true);
        let task = runtime().spawn(async move {
            let client = Self::http_client();
            obscure_core::subscription::fetch(&client, &url).await
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let result = match task.await {
                    Ok(Ok(fetched)) => {
                        let count = fetched.servers.len();
                        let was_selected = this.imp().profiles.borrow().selected.clone();
                        this.imp()
                            .profiles
                            .borrow_mut()
                            .update_subscription(&id, &fetched);
                        this.save_profiles();
                        this.sync_from_profiles();
                        if this.connected() && this.imp().profiles.borrow().selected != was_selected
                        {
                            this.reconnect();
                        }
                        Ok(count)
                    }
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(Error::Subscription("task cancelled".into())),
                };
                this.set_subscription_updating(&id, false);
                done(result);
            }
        ));
    }

    fn set_subscription_updating(&self, id: &str, updating: bool) {
        let store = &self.imp().subscriptions;
        for i in 0..store.n_items() {
            if let Some(obj) = store.item(i).and_downcast::<SubscriptionObject>()
                && obj.id() == id
            {
                obj.set_updating(updating);
            }
        }
    }

    pub fn remove_subscription(&self, id: &str) {
        let selected_before = self.imp().profiles.borrow().selected.clone();
        if self.imp().profiles.borrow_mut().remove_subscription(id) {
            self.save_profiles();
            self.sync_from_profiles();
            if self.connected() && self.imp().profiles.borrow().selected != selected_before {
                self.disconnect();
            }
        }
    }

    /// Updates every subscription whose interval has elapsed (startup).
    pub fn refresh_due_subscriptions(&self) {
        let due: Vec<String> = self
            .imp()
            .profiles
            .borrow()
            .subscriptions
            .iter()
            .filter(|s| s.needs_update())
            .map(|s| s.id.clone())
            .collect();
        for id in due {
            tracing::info!("subscription {id} is due for update");
            self.update_subscription(id, |r| {
                if let Err(e) = r {
                    tracing::warn!("automatic subscription update failed: {e}");
                }
            });
        }
    }

    // -- latency --------------------------------------------------------------

    /// TCP-pings every server; results land in the list model. `done` gets
    /// the id of the fastest server, if any.
    pub fn test_all_latency(&self, done: impl FnOnce(Option<String>) + 'static) {
        if self.testing() {
            return;
        }
        let entries: Vec<(String, Server)> = self
            .imp()
            .profiles
            .borrow()
            .servers
            .iter()
            .map(|e| (e.id.clone(), e.server.clone()))
            .collect();
        if entries.is_empty() {
            done(None);
            return;
        }
        self.set_testing(true);
        for (id, _) in &entries {
            self.set_latency(id, LATENCY_TESTING);
        }
        let servers: Vec<Server> = entries.iter().map(|(_, s)| s.clone()).collect();
        let task = runtime().spawn(async move {
            obscure_core::latency::tcp_ping_all(&servers, 8, Duration::from_secs(4)).await
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let results = task.await.unwrap_or_default();
                let mut best: Option<(u32, String)> = None;
                for ((id, _), r) in entries.iter().zip(results) {
                    let ms = match r {
                        Ok(ms) => {
                            if best.as_ref().is_none_or(|(b, _)| ms < *b) {
                                best = Some((ms, id.clone()));
                            }
                            ms as i32
                        }
                        Err(_) => LATENCY_FAILED,
                    };
                    this.set_latency(id, ms);
                }
                this.set_testing(false);
                done(best.map(|(_, id)| id));
            }
        ));
    }

    fn set_latency(&self, id: &str, ms: i32) {
        self.imp().latency.borrow_mut().insert(id.to_owned(), ms);
        let store = &self.imp().servers;
        for i in 0..store.n_items() {
            if let Some(obj) = store.item(i).and_downcast::<ServerObject>()
                && obj.id() == id
            {
                obj.set_latency_ms(ms);
            }
        }
    }

    pub fn set_auto_select_from_ui(&self, auto: bool) {
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.auto_select != auto;
            p.auto_select = auto;
            changed
        };
        if changed {
            self.save_profiles();
            self.set_auto_select(auto);
        }
    }

    pub fn servers(&self) -> &gio::ListStore {
        &self.imp().servers
    }

    /// Adds servers; returns how many were new.
    pub fn add_servers(&self, servers: Vec<Server>) -> usize {
        let added = self.imp().profiles.borrow_mut().add_servers(servers);
        self.save_profiles();
        self.sync_from_profiles();
        self.resolve_countries();
        added
    }

    /// Looks up the country of every server that does not have one yet,
    /// offline, using the geoip.dat shipped with Xray. Runs in the
    /// background; the list refreshes when done.
    pub fn resolve_countries(&self) {
        let pending: Vec<(String, String, u16)> = self
            .imp()
            .profiles
            .borrow()
            .servers
            .iter()
            .filter(|e| e.country.is_none())
            .map(|e| (e.id.clone(), e.server.address.clone(), e.server.port))
            .collect();
        if pending.is_empty() {
            return;
        }
        let geoip_path = paths::core_dir().join("geoip.dat");
        if !geoip_path.exists() {
            return;
        }
        let task = runtime().spawn(async move {
            let db = match geoip_db(&geoip_path).await {
                Some(db) => db,
                None => return Vec::new(),
            };
            let mut out = Vec::new();
            for (id, host, port) in pending {
                let ip = match host.parse::<std::net::IpAddr>() {
                    Ok(ip) => Some(ip),
                    Err(_) => tokio::time::timeout(
                        Duration::from_secs(5),
                        tokio::net::lookup_host((host.as_str(), port)),
                    )
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .and_then(|mut addrs| addrs.next())
                    .map(|a| a.ip()),
                };
                let country = ip.and_then(|ip| db.lookup(ip).map(str::to_owned));
                out.push((id, country));
            }
            out
        });
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let Ok(results) = task.await else { return };
                let mut changed = false;
                {
                    let mut p = this.imp().profiles.borrow_mut();
                    for (id, country) in results {
                        if country.is_some() && p.set_country(&id, country) {
                            changed = true;
                        }
                    }
                }
                if changed {
                    this.save_profiles();
                    this.sync_from_profiles();
                }
            }
        ));
    }

    pub fn remove_server(&self, id: &str) {
        let was_selected = self.imp().profiles.borrow().selected.as_deref() == Some(id);
        self.imp().profiles.borrow_mut().remove(id);
        self.save_profiles();
        self.sync_from_profiles();
        if was_selected && self.connected() {
            self.disconnect();
        }
    }

    pub fn select_server(&self, id: &str) {
        {
            let mut p = self.imp().profiles.borrow_mut();
            if p.get(id).is_none() || p.selected.as_deref() == Some(id) {
                return;
            }
            p.selected = Some(id.to_owned());
        }
        self.save_profiles();
        self.sync_from_profiles();
        if self.connected() || self.busy() {
            // Switching server while connected: reconnect to the new one.
            self.reconnect();
        }
    }

    pub fn set_apply_mode_from_ui(&self, mode: &str) {
        let Some(mode) = str_to_mode(mode) else {
            return;
        };
        let changed = {
            let mut p = self.imp().profiles.borrow_mut();
            let changed = p.apply_mode != mode;
            p.apply_mode = mode;
            changed
        };
        if changed {
            self.save_profiles();
            self.set_apply_mode(mode_to_str(mode));
            if self.connected() {
                self.reconnect();
            }
        }
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.imp().log.borrow().iter().cloned().collect()
    }

    pub fn last_error(&self) -> Option<String> {
        self.imp().last_error.borrow().clone()
    }

    pub fn access_events(&self) -> Vec<AccessEvent> {
        self.imp().access.borrow().iter().cloned().collect()
    }

    fn push_log(&self, line: String) {
        if let Some(ev) = parse_access_line(&line) {
            {
                let mut access = self.imp().access.borrow_mut();
                if access.len() >= MAX_ACCESS_EVENTS {
                    access.pop_front();
                }
                access.push_back(ev.clone());
            }
            self.emit_by_name::<()>("access-event", &[&AccessEventBox(ev)]);
        }
        {
            let mut log = self.imp().log.borrow_mut();
            if log.len() >= MAX_LOG_LINES {
                log.pop_front();
            }
            log.push_back(line.clone());
        }
        self.emit_by_name::<()>("log-line", &[&line]);
    }

    pub fn clear_log(&self) {
        self.imp().log.borrow_mut().clear();
    }

    // -- connection ---------------------------------------------------------

    /// Restores a system proxy left behind by a crash. Returns `true` if
    /// something was restored.
    pub fn recover_from_crash(&self) -> bool {
        let sysproxy = SysProxy::new(paths::state_file());
        if !sysproxy.is_dirty() {
            return false;
        }
        tracing::warn!("previous session left the system proxy applied; restoring");
        match runtime().block_on(sysproxy.restore()) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("restore failed: {e}");
                false
            }
        }
    }

    pub fn toggle(&self) {
        if self.connected() || self.busy() {
            self.disconnect();
        } else {
            self.connect();
        }
    }

    fn reconnect(&self) {
        self.disconnect();
        // The actor sends `Disconnected` when done; connect again after it.
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let mut tries = 0;
                while (this.busy() || this.connected()) && tries < 50 {
                    glib::timeout_future(Duration::from_millis(100)).await;
                    tries += 1;
                }
                this.connect();
            }
        ));
    }

    pub fn connect(&self) {
        if self.busy() || self.connected() {
            return;
        }
        if self.imp().profiles.borrow().auto_select
            && self.imp().profiles.borrow().servers.len() > 1
        {
            // Pick the fastest server first, then connect to it.
            self.set_busy(true);
            self.set_status_text(gettext("Finding the fastest server…"));
            self.test_all_latency(clone!(
                #[weak(rename_to = this)]
                self,
                move |best| {
                    this.set_busy(false);
                    if let Some(id) = best {
                        let mut p = this.imp().profiles.borrow_mut();
                        p.selected = Some(id);
                        drop(p);
                        this.save_profiles();
                        this.sync_from_profiles();
                    }
                    this.connect_selected();
                }
            ));
            return;
        }
        self.connect_selected();
    }

    fn connect_selected(&self) {
        if self.busy() || self.connected() {
            return;
        }
        let Some(entry) = self.imp().profiles.borrow().selected_entry().cloned() else {
            self.set_status_text(gettext("Choose a server to connect"));
            return;
        };
        let (
            local_port,
            preset,
            listed,
            apply_mode,
            dns,
            prerelease,
            custom_rules,
            config_override,
        ) = {
            let p = self.imp().profiles.borrow();
            (
                p.local_port,
                p.route_preset,
                p.listed_domains.clone(),
                p.apply_mode,
                p.dns.clone(),
                p.prerelease,
                p.custom_rules.clone(),
                p.config_override.clone(),
            )
        };

        self.set_busy(true);
        *self.imp().last_error.borrow_mut() = None;
        self.set_status_text(gettext("Connecting…"));

        let params = SessionParams {
            server: entry.server.clone(),
            local_port,
            route_preset: preset,
            listed_domains: listed,
            apply_mode,
            dns,
            prerelease,
            custom_rules,
            config_override,
        };

        let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(4);
        *self.imp().cmd_tx.borrow_mut() = Some(cmd_tx);

        let user_agent = obscure_core::user_agent(obscure_core::APP_VERSION);
        runtime().spawn(actor(params, user_agent, ui_tx, cmd_rx));

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                while let Ok(ev) = ui_rx.recv().await {
                    this.handle_event(ev);
                }
            }
        ));
    }

    pub fn disconnect(&self) {
        let tx = self.imp().cmd_tx.borrow().clone();
        if let Some(tx) = tx {
            self.set_busy(true);
            self.set_status_text(gettext("Disconnecting…"));
            runtime().spawn(async move {
                let _ = tx.send(Cmd::Stop).await;
            });
        }
    }

    /// Synchronous disconnect for application shutdown.
    pub fn shutdown(&self) {
        let tx = self.imp().cmd_tx.borrow_mut().take();
        if let Some(tx) = tx {
            runtime().block_on(async move {
                let _ = tx.send(Cmd::Stop).await;
                // Give the actor time to restore the proxy and stop Xray.
                let _ = tokio::time::timeout(Duration::from_secs(5), tx.closed()).await;
            });
        }
    }

    fn handle_event(&self, ev: UiEvent) {
        match ev {
            UiEvent::Progress(p) => match p {
                Progress::Resolving => {
                    self.set_status_text(gettext("Preparing the connection engine…"));
                }
                Progress::Downloading { received, total } => {
                    self.set_progress_visible(true);
                    let frac = total
                        .filter(|t| *t > 0)
                        .map(|t| received as f64 / t as f64)
                        .unwrap_or(0.0);
                    self.set_progress(frac);
                    let mb = received as f64 / 1_048_576.0;
                    let text = match total {
                        Some(t) => gettext("Downloading the connection engine: %a of %b MB")
                            .replace("%a", &format!("{mb:.0}"))
                            .replace("%b", &format!("{:.0}", t as f64 / 1_048_576.0)),
                        None => gettext("Downloading the connection engine: %a MB")
                            .replace("%a", &format!("{mb:.0}")),
                    };
                    self.set_status_text(text);
                }
                Progress::Verifying => self.set_status_text(gettext("Verifying the download…")),
                Progress::Extracting => {
                    self.set_status_text(gettext("Installing the connection engine…"))
                }
                Progress::Done { version } => {
                    self.set_progress_visible(false);
                    self.push_log(format!("Xray {version} installed"));
                }
            },
            UiEvent::Log(line) => self.push_log(line),
            UiEvent::Restarting(attempt) => {
                self.set_connected(false);
                self.set_busy(true);
                self.set_traffic_text(String::new());
                self.set_status_text(gettext("Connecting…"));
                self.push_log(format!("restarting core (attempt {attempt})"));
            }
            UiEvent::Traffic(sample) => {
                self.set_traffic_text(format!(
                    "↑ {} · ↓ {} · {}",
                    format_rate(sample.up_rate),
                    format_rate(sample.down_rate),
                    format_bytes(sample.total.uplink + sample.total.downlink)
                ));
            }
            UiEvent::Connected => {
                self.set_progress_visible(false);
                self.set_busy(false);
                self.set_connected(true);
                let mode = self.imp().profiles.borrow().apply_mode;
                self.set_status_text(match mode {
                    ApplyMode::SystemProxy => gettext("Connected · system proxy active"),
                    ApplyMode::LocalOnly => gettext("Connected · local proxy only"),
                    ApplyMode::Tunnel => gettext("Connected · all traffic through the tunnel"),
                });
                self.emit_by_name::<()>(
                    "notification",
                    &[&gettext("Connected"), &self.selected_name(), &false],
                );
            }
            UiEvent::Disconnected => {
                self.finish_disconnected();
                self.set_status_text(gettext("Disconnected"));
            }
            UiEvent::Failed(err) => {
                let msg = error_message(&err);
                tracing::error!("connection failed: {err}");
                self.push_log(format!("error: {err}"));
                self.finish_disconnected();
                *self.imp().last_error.borrow_mut() = Some(msg.clone());
                self.set_status_text(msg.clone());
                self.emit_by_name::<()>("failed", &[]);
                self.emit_by_name::<()>(
                    "notification",
                    &[&gettext("Connection failed"), &msg, &true],
                );
            }
        }
    }

    fn finish_disconnected(&self) {
        *self.imp().cmd_tx.borrow_mut() = None;
        self.set_traffic_text(String::new());
        self.set_progress_visible(false);
        self.set_busy(false);
        self.set_connected(false);
    }
}

fn mode_to_str(m: ApplyMode) -> &'static str {
    match m {
        ApplyMode::LocalOnly => "local_only",
        ApplyMode::SystemProxy => "system_proxy",
        ApplyMode::Tunnel => "tunnel",
    }
}

fn preset_to_str(p: RoutePreset) -> &'static str {
    match p {
        RoutePreset::All => "all",
        RoutePreset::Smart => "smart",
        RoutePreset::OnlyListed => "only_listed",
    }
}

fn str_to_preset(s: &str) -> Option<RoutePreset> {
    match s {
        "all" => Some(RoutePreset::All),
        "smart" => Some(RoutePreset::Smart),
        "only_listed" => Some(RoutePreset::OnlyListed),
        _ => None,
    }
}

fn str_to_mode(s: &str) -> Option<ApplyMode> {
    match s {
        "local_only" => Some(ApplyMode::LocalOnly),
        "system_proxy" => Some(ApplyMode::SystemProxy),
        "tunnel" => Some(ApplyMode::Tunnel),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// tokio side

/// Owns the Xray process for one session. Ensures the core is installed,
/// starts it, applies the system proxy, polls traffic stats, restarts the
/// core with backoff if it dies, and always restores the system proxy on
/// the way out.
async fn actor(
    params: SessionParams,
    user_agent: String,
    ui: async_channel::Sender<UiEvent>,
    mut cmds: tokio::sync::mpsc::Receiver<Cmd>,
) {
    let send = |ev: UiEvent| {
        let _ = ui.send_blocking(ev);
    };

    let manager = CoreManager::new(paths::core_dir(), &user_agent);
    let ui_progress = ui.clone();
    let ensure = manager.ensure_installed(params.prerelease, move |p| {
        let _ = ui_progress.send_blocking(UiEvent::Progress(p));
    });
    // Allow cancelling during a long download.
    let installed = tokio::select! {
        r = ensure => r,
        _ = cmds.recv() => {
            send(UiEvent::Disconnected);
            return;
        }
    };
    if let Err(e) = installed {
        send(UiEvent::Failed(e));
        return;
    }

    let api_port = match obscure_core::latency::free_port().await {
        Ok(p) => p,
        Err(e) => {
            send(UiEvent::Failed(e));
            return;
        }
    };
    let tunnel = params.apply_mode == ApplyMode::Tunnel;
    let cfg = match &params.config_override {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(mut v) => {
                // Keep stats reachable even with a custom config.
                v["api"] = serde_json::json!({ "tag": "api", "listen": format!("127.0.0.1:{api_port}"), "services": ["StatsService"] });
                if v.get("stats").is_none() {
                    v["stats"] = serde_json::json!({});
                }
                v
            }
            Err(e) => {
                send(UiEvent::Failed(Error::InvalidOverride(e.to_string())));
                return;
            }
        },
        None => {
            let mut opts = ConfigOptions::new(&params.server);
            opts.local_port = params.local_port;
            opts.route_preset = params.route_preset;
            opts.listed_domains = &params.listed_domains;
            opts.dns = &params.dns;
            opts.api_port = Some(api_port);
            opts.tunnel = tunnel;
            opts.custom_rules = &params.custom_rules;
            build(&opts)
        }
    };
    let cfg_path = paths::runtime_config_file();
    if let Err(e) = write_config(&cfg_path, &cfg) {
        send(UiEvent::Failed(e));
        return;
    }

    // Tunnel mode: the binary needs file capabilities. Ask once through
    // polkit; a core update replaces the binary, so this is re-checked on
    // every connection.
    if tunnel {
        let xray = manager.xray_path();
        let status = capabilities(&xray);
        if status != CapStatus::Granted {
            send(UiEvent::Log(format!(
                "tunnel: capabilities {status:?}, requesting grant"
            )));
            send(UiEvent::Progress(Progress::Resolving));
            match grant(&helper_path(), &xray).await {
                Ok(CapStatus::Granted) => send(UiEvent::Log("tunnel: capabilities granted".into())),
                Ok(other) => {
                    send(UiEvent::Failed(Error::Tun(format!(
                        "capabilities still {other:?} after grant"
                    ))));
                    return;
                }
                Err(e) => {
                    send(UiEvent::Failed(e));
                    return;
                }
            }
        }
    }

    let supervisor = Supervisor::new(manager.xray_path(), manager.dir().to_path_buf());
    let mut core: RunningCore = match supervisor.start(&cfg_path, params.local_port).await {
        Ok(c) => c,
        Err(e) => {
            send(UiEvent::Failed(e));
            return;
        }
    };

    let sysproxy = SysProxy::new(paths::state_file());
    let mut proxy_applied = false;
    if params.apply_mode == ApplyMode::SystemProxy {
        match sysproxy.apply(params.local_port).await {
            Ok(()) => proxy_applied = true,
            Err(e) => {
                // Not fatal: the local proxy works; tell the user via log/status.
                tracing::warn!("system proxy not applied: {e}");
                send(UiEvent::Log(format!("system proxy not applied: {e}")));
            }
        }
    }
    send(UiEvent::Connected);

    // Refresh geo data in the background at most once a day.
    if manager.geo_needs_update() {
        let manager = manager.clone();
        let ui_geo = ui.clone();
        tokio::spawn(async move {
            match manager.update_geo().await {
                Ok(()) => {
                    let _ = ui_geo.send_blocking(UiEvent::Log("geo data updated".into()));
                }
                Err(e) => {
                    let _ = ui_geo.send_blocking(UiEvent::Log(format!("geo update failed: {e}")));
                }
            }
        });
    }

    let mut stats = StatsClient::new(api_port).ok();
    let mut meter = RateMeter::default();
    let mut policy = RestartPolicy::default();
    let mut started_at = tokio::time::Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let outcome = loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Stop) | None => break Ok(()),
            },
            ev = core.events.recv() => match ev {
                Some(CoreEvent::Log(line)) => send(UiEvent::Log(line)),
                Some(CoreEvent::Exited { .. }) | None => {}
            },
            _ = tick.tick() => {
                if let Some(status) = core.check_exit() {
                    // Unexpected exit: restart with backoff or give up.
                    let mut output = Vec::new();
                    while let Ok(CoreEvent::Log(l)) = core.events.try_recv() {
                        output.push(l);
                    }
                    if started_at.elapsed() > Duration::from_secs(60) {
                        policy.reset();
                    }
                    let Some(delay) = policy.next_delay() else {
                        break Err(Error::CoreExited { status, output: output.join("\n") });
                    };
                    send(UiEvent::Log(format!("core exited ({status}); restarting in {delay:?}")));
                    send(UiEvent::Restarting(policy.attempts()));
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cmds.recv() => break Ok(()),
                    }
                    match supervisor.start(&cfg_path, params.local_port).await {
                        Ok(c) => {
                            core = c;
                            started_at = tokio::time::Instant::now();
                            meter = RateMeter::default();
                            send(UiEvent::Connected);
                        }
                        Err(e) => break Err(e),
                    }
                    continue;
                }
                if let Some(client) = stats.as_mut()
                    && let Ok(traffic) = client.proxy_traffic().await
                {
                    send(UiEvent::Traffic(meter.update(traffic)));
                }
            }
        }
    };

    if proxy_applied && let Err(e) = sysproxy.restore().await {
        tracing::error!("restoring system proxy failed: {e}");
        send(UiEvent::Log(format!("restoring system proxy failed: {e}")));
    }

    match outcome {
        Ok(()) => {
            core.stop().await;
            send(UiEvent::Disconnected);
        }
        Err(e) => {
            core.stop().await;
            send(UiEvent::Failed(e));
        }
    }
}

/// The geoip database is ~17 MB; load it once per process, off the UI thread.
async fn geoip_db(path: &std::path::Path) -> Option<std::sync::Arc<obscure_core::geoip::GeoIpDb>> {
    use std::sync::{Arc, OnceLock};
    use tokio::sync::Mutex;
    static DB: OnceLock<Mutex<Option<Arc<obscure_core::geoip::GeoIpDb>>>> = OnceLock::new();
    let slot = DB.get_or_init(|| Mutex::new(None));
    let mut guard = slot.lock().await;
    if let Some(db) = guard.as_ref() {
        return Some(db.clone());
    }
    let path = path.to_path_buf();
    let db = tokio::task::spawn_blocking(move || obscure_core::geoip::GeoIpDb::load(&path))
        .await
        .ok()?
        .map_err(|e| tracing::warn!("geoip: {e}"))
        .ok()?;
    let db = Arc::new(db);
    *guard = Some(db.clone());
    Some(db)
}

/// Installed helper if present, otherwise the one from the build tree.
fn helper_path() -> std::path::PathBuf {
    let installed = std::path::Path::new(crate::config::LIBEXECDIR).join("obscure-helper");
    if installed.exists() {
        return installed;
    }
    let build = std::path::Path::new(crate::config::BUILD_HELPER);
    if !crate::config::BUILD_HELPER.is_empty() && build.exists() {
        return build.to_path_buf();
    }
    installed
}
