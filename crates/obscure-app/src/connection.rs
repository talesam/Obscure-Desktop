//! Connection state machine exposed to the UI as a GObject, plus the tokio
//! actor that owns the Xray process while connected.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::Duration;

use gettextrs::gettext;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib::clone};
use obscure_core::config::{ConfigOptions, build};
use obscure_core::core_manager::{CoreManager, Progress};
use obscure_core::links::Server;
use obscure_core::profile::{ApplyMode, Profiles, RoutePreset};
use obscure_core::stats::{RateMeter, StatsClient, TrafficSample, format_bytes, format_rate};
use obscure_core::supervisor::{CoreEvent, RestartPolicy, RunningCore, Supervisor, write_config};
use obscure_core::sysproxy::SysProxy;
use obscure_core::{Error, paths};

use crate::humanize::error_message;
use crate::runtime::runtime;
use crate::server_object::ServerObject;

const MAX_LOG_LINES: usize = 500;

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

        pub profiles: RefCell<Profiles>,
        pub servers: gio::ListStore,
        pub log: RefCell<VecDeque<String>>,
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
                profiles: RefCell::new(Profiles::default()),
                servers: gio::ListStore::new::<ServerObject>(),
                log: RefCell::new(VecDeque::new()),
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
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("failed").build()])
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
    }

    fn save_profiles(&self) {
        if let Err(e) = self.imp().profiles.borrow().save(&paths::profiles_file()) {
            tracing::error!("cannot save profiles: {e}");
        }
    }

    /// Rebuilds the list model and derived properties from `profiles`.
    fn sync_from_profiles(&self) {
        let imp = self.imp();
        let profiles = imp.profiles.borrow();
        let objects: Vec<ServerObject> = profiles
            .servers
            .iter()
            .map(|e| ServerObject::from_entry(e, profiles.selected.as_deref() == Some(&e.id)))
            .collect();
        imp.servers.remove_all();
        imp.servers.extend_from_slice(&objects);
        self.set_has_servers(!profiles.servers.is_empty());
        self.set_selected_name(
            profiles
                .selected_entry()
                .map(|e| e.server.name.clone())
                .unwrap_or_default(),
        );
        self.set_apply_mode(mode_to_str(profiles.apply_mode));
    }

    pub fn servers(&self) -> &gio::ListStore {
        &self.imp().servers
    }

    /// Adds servers; returns how many were new.
    pub fn add_servers(&self, servers: Vec<Server>) -> usize {
        let added = self.imp().profiles.borrow_mut().add_servers(servers);
        self.save_profiles();
        self.sync_from_profiles();
        added
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

    fn push_log(&self, line: String) {
        let mut log = self.imp().log.borrow_mut();
        if log.len() >= MAX_LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
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
        let Some(entry) = self.imp().profiles.borrow().selected_entry().cloned() else {
            self.set_status_text(gettext("Choose a server to connect"));
            return;
        };
        let (local_port, preset, listed, apply_mode) = {
            let p = self.imp().profiles.borrow();
            (
                p.local_port,
                p.route_preset,
                p.listed_domains.clone(),
                p.apply_mode,
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
        };

        let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Cmd>(4);
        *self.imp().cmd_tx.borrow_mut() = Some(cmd_tx);

        let user_agent = obscure_core::user_agent(crate::config::VERSION);
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
                    ApplyMode::Tunnel => gettext("Connected"),
                });
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
                self.set_status_text(msg);
                self.emit_by_name::<()>("failed", &[]);
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
    let ensure = manager.ensure_installed(false, move |p| {
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
    let cfg = {
        let mut opts = ConfigOptions::new(&params.server);
        opts.local_port = params.local_port;
        opts.route_preset = params.route_preset;
        opts.listed_domains = &params.listed_domains;
        opts.api_port = Some(api_port);
        build(&opts)
    };
    let cfg_path = paths::runtime_config_file();
    if let Err(e) = write_config(&cfg_path, &cfg) {
        send(UiEvent::Failed(e));
        return;
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
