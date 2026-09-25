//! Persistent user data: servers, selection and connection preferences.
//! Stored as JSON in `~/.config/obscure/profiles.json` with a `version`
//! field for future migrations.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::links::Server;
use crate::subscription::{Fetched, UserInfo};

pub const CURRENT_VERSION: u32 = 1;

/// How connecting should affect the rest of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    /// Only the local proxy on 127.0.0.1 is started; nothing else changes.
    LocalOnly,
    /// Local proxy + desktop system proxy settings (GNOME/KDE). No root.
    #[default]
    SystemProxy,
    /// TUN device: all traffic. Requires the privileged helper (phase 4).
    Tunnel,
}

/// Where a custom rule sends matching traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTarget {
    Proxy,
    Direct,
    Block,
}

/// A user rule: a domain, an IP/CIDR, or a `geosite:`/`geoip:` category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRule {
    pub pattern: String,
    pub target: RuleTarget,
}

/// Which traffic goes through the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutePreset {
    /// Everything except private networks.
    All,
    /// Private → direct, ads → block, rest → proxy.
    #[default]
    Smart,
    /// Only user-listed domains → proxy.
    OnlyListed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    pub id: String,
    /// Subscription this server belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// ISO 3166-1 alpha-2 of the server's IP, looked up offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(flatten)]
    pub server: Server,
}

impl ServerEntry {
    pub fn new(server: Server) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            group: None,
            country: None,
            server,
        }
    }
}

/// A subscription: a URL that yields a group of servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub url: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_info: Option<UserInfo>,
    /// Unix timestamp of the last successful update.
    #[serde(default)]
    pub last_update: u64,
    /// Refresh interval in seconds (from the server or the default).
    #[serde(default = "default_update_interval")]
    pub update_interval: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_page_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
}

pub const DEFAULT_UPDATE_INTERVAL: u64 = 24 * 3600;

fn default_update_interval() -> u64 {
    DEFAULT_UPDATE_INTERVAL
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Subscription {
    pub fn needs_update(&self) -> bool {
        now().saturating_sub(self.last_update) >= self.update_interval
    }
}

/// Returns `true` if the text is an `http(s)://` URL (a subscription).
pub fn looks_like_subscription_url(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("http://") || t.starts_with("https://")) && !t.contains(char::is_whitespace)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    #[serde(default)]
    pub subscriptions: Vec<Subscription>,
    #[serde(default)]
    pub selected: Option<String>,
    /// Connect to the server with the lowest latency instead of `selected`.
    #[serde(default)]
    pub auto_select: bool,
    #[serde(default)]
    pub apply_mode: ApplyMode,
    #[serde(default)]
    pub route_preset: RoutePreset,
    #[serde(default = "default_local_port")]
    pub local_port: u16,
    /// Domains routed through the proxy in `OnlyListed` mode.
    #[serde(default)]
    pub listed_domains: Vec<String>,
    /// DNS-over-HTTPS server used for remote resolution.
    #[serde(default = "default_dns")]
    pub dns: String,
    /// Follow Xray pre-releases instead of stable.
    #[serde(default)]
    pub prerelease: bool,
    /// User rules evaluated before the preset.
    #[serde(default)]
    pub custom_rules: Vec<RouteRule>,
    /// Raw Xray JSON that replaces the generated configuration when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_override: Option<String>,
}

pub const DEFAULT_DNS: &str = "https://1.1.1.1/dns-query";

fn default_dns() -> String {
    DEFAULT_DNS.to_owned()
}

fn default_local_port() -> u16 {
    crate::DEFAULT_LOCAL_PORT
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            servers: Vec::new(),
            subscriptions: Vec::new(),
            selected: None,
            auto_select: false,
            apply_mode: ApplyMode::default(),
            route_preset: RoutePreset::default(),
            local_port: default_local_port(),
            listed_domains: Vec::new(),
            dns: default_dns(),
            prerelease: false,
            custom_rules: Vec::new(),
            config_override: None,
        }
    }
}

impl Profiles {
    /// Loads from `path`; a missing file yields the default (empty) profile.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut p: Profiles = serde_json::from_slice(&bytes)?;
                p.migrate();
                Ok(p)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Atomically writes to `path` with mode 0600 (it holds credentials).
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_private(path, &serde_json::to_vec_pretty(self)?)
    }

    fn migrate(&mut self) {
        // Future versions bump CURRENT_VERSION and transform fields here.
        self.version = CURRENT_VERSION;
        // Drop servers whose subscription no longer exists.
        let ids: Vec<String> = self.subscriptions.iter().map(|s| s.id.clone()).collect();
        self.servers
            .retain(|e| e.group.as_ref().is_none_or(|g| ids.contains(g)));
        if let Some(sel) = &self.selected
            && !self.servers.iter().any(|s| &s.id == sel)
        {
            self.selected = None;
        }
    }

    /// Adds servers, skipping exact duplicates. Returns how many were added.
    /// The first added server becomes selected if nothing was.
    pub fn add_servers(&mut self, servers: impl IntoIterator<Item = Server>) -> usize {
        let mut added = 0;
        for server in servers {
            if self.servers.iter().any(|e| e.server == server) {
                continue;
            }
            let entry = ServerEntry::new(server);
            if self.selected.is_none() {
                self.selected = Some(entry.id.clone());
            }
            self.servers.push(entry);
            added += 1;
        }
        added
    }

    /// Adds a subscription from a successful fetch. Servers are tagged with
    /// the subscription id. Returns the new subscription's id.
    pub fn add_subscription(&mut self, url: &str, fetched: &Fetched) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        let name = fetched
            .title
            .clone()
            .unwrap_or_else(|| host_of(url).unwrap_or_else(|| url.to_owned()));
        self.subscriptions.push(Subscription {
            id: id.clone(),
            url: fetched.new_url.clone().unwrap_or_else(|| url.to_owned()),
            name,
            user_info: fetched.user_info,
            last_update: now(),
            update_interval: fetched
                .update_interval
                .map(|d| d.as_secs())
                .unwrap_or(DEFAULT_UPDATE_INTERVAL),
            web_page_url: fetched.web_page_url.clone(),
            support_url: fetched.support_url.clone(),
        });
        self.replace_group_servers(&id, &fetched.servers);
        id
    }

    /// Applies a fresh fetch to an existing subscription: metadata is
    /// refreshed and the server list replaced. The selected server survives
    /// if an identical server is still present.
    pub fn update_subscription(&mut self, id: &str, fetched: &Fetched) -> bool {
        let Some(sub) = self.subscriptions.iter_mut().find(|s| s.id == id) else {
            return false;
        };
        if let Some(t) = &fetched.title {
            sub.name = t.clone();
        }
        if let Some(u) = &fetched.new_url {
            sub.url = u.clone();
        }
        sub.user_info = fetched.user_info.or(sub.user_info);
        sub.last_update = now();
        if let Some(d) = fetched.update_interval {
            sub.update_interval = d.as_secs();
        }
        sub.web_page_url = fetched.web_page_url.clone().or(sub.web_page_url.take());
        sub.support_url = fetched.support_url.clone().or(sub.support_url.take());
        self.replace_group_servers(id, &fetched.servers);
        true
    }

    fn replace_group_servers(&mut self, group: &str, servers: &[Server]) {
        let selected_server = self.selected_entry().map(|e| e.server.clone());
        let old: Vec<ServerEntry> = self
            .servers
            .iter()
            .filter(|e| e.group.as_deref() == Some(group))
            .cloned()
            .collect();
        self.servers.retain(|e| e.group.as_deref() != Some(group));
        for server in servers {
            // Keep ids stable for unchanged servers so the UI selection holds.
            let id = old
                .iter()
                .find(|e| e.server == *server)
                .map(|e| e.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let country = old
                .iter()
                .find(|e| e.server == *server)
                .and_then(|e| e.country.clone());
            self.servers.push(ServerEntry {
                id,
                group: Some(group.to_owned()),
                country,
                server: server.clone(),
            });
        }
        // Restore or fix the selection.
        let still_there = selected_server
            .as_ref()
            .and_then(|s| self.servers.iter().find(|e| e.server == *s))
            .map(|e| e.id.clone());
        self.selected = still_there.or_else(|| self.servers.first().map(|e| e.id.clone()));
    }

    pub fn remove_subscription(&mut self, id: &str) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| s.id != id);
        if self.subscriptions.len() == before {
            return false;
        }
        self.servers.retain(|e| e.group.as_deref() != Some(id));
        if self.selected_entry().is_none() {
            self.selected = self.servers.first().map(|e| e.id.clone());
        }
        true
    }

    pub fn subscription(&self, id: &str) -> Option<&Subscription> {
        self.subscriptions.iter().find(|s| s.id == id)
    }

    pub fn servers_in_group(&self, id: &str) -> usize {
        self.servers
            .iter()
            .filter(|e| e.group.as_deref() == Some(id))
            .count()
    }

    pub fn remove(&mut self, id: &str) -> Option<ServerEntry> {
        let pos = self.servers.iter().position(|e| e.id == id)?;
        let removed = self.servers.remove(pos);
        if self.selected.as_deref() == Some(id) {
            self.selected = self.servers.first().map(|e| e.id.clone());
        }
        Some(removed)
    }

    /// Renames a server; returns `false` if it does not exist or the name is blank.
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        match self.servers.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.server.name = name.to_owned();
                true
            }
            None => false,
        }
    }

    pub fn set_country(&mut self, id: &str, country: Option<String>) -> bool {
        match self.servers.iter_mut().find(|e| e.id == id) {
            Some(e) => {
                e.country = country;
                true
            }
            None => false,
        }
    }

    pub fn get(&self, id: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|e| e.id == id)
    }

    pub fn selected_entry(&self) -> Option<&ServerEntry> {
        self.selected.as_deref().and_then(|id| self.get(id))
    }
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::parse_link;

    fn fetched(names: &[u16]) -> Fetched {
        Fetched {
            servers: names.iter().map(|n| sample(*n)).collect(),
            failed: vec![],
            user_info: Some(UserInfo {
                upload: Some(1),
                download: Some(2),
                total: Some(10),
                expire: None,
            }),
            title: Some("Panel".into()),
            update_interval: Some(std::time::Duration::from_secs(3600)),
            web_page_url: None,
            support_url: None,
            new_url: None,
        }
    }

    #[test]
    fn subscription_lifecycle_keeps_selection() {
        let mut p = Profiles::default();
        p.add_servers([sample(1)]);
        let id = p.add_subscription("https://x.example/sub", &fetched(&[10, 11, 12]));
        assert_eq!(p.servers.len(), 4);
        assert_eq!(p.servers_in_group(&id), 3);
        assert_eq!(p.subscription(&id).unwrap().name, "Panel");
        assert_eq!(p.subscription(&id).unwrap().update_interval, 3600);
        assert!(!p.subscription(&id).unwrap().needs_update());

        // Select a subscription server, then update: 11 stays, 10 goes, 13 arrives.
        let sel = p
            .servers
            .iter()
            .find(|e| e.server.name == "S11")
            .unwrap()
            .id
            .clone();
        p.selected = Some(sel.clone());
        assert!(p.update_subscription(&id, &fetched(&[11, 12, 13])));
        assert_eq!(p.servers_in_group(&id), 3);
        assert_eq!(
            p.selected.as_deref(),
            Some(sel.as_str()),
            "id of unchanged server is stable"
        );
        assert!(p.servers.iter().all(|e| e.server.name != "S10"));

        // Removing the subscription removes its servers and fixes selection.
        assert!(p.remove_subscription(&id));
        assert_eq!(p.servers.len(), 1);
        assert_eq!(p.selected, Some(p.servers[0].id.clone()));
        assert!(!p.remove_subscription(&id));
    }

    #[test]
    fn subscription_url_detection_and_host_name() {
        assert!(looks_like_subscription_url(
            " https://x.example/sub?token=1 "
        ));
        assert!(!looks_like_subscription_url("vless://x@h:1"));
        assert!(!looks_like_subscription_url("https://x.example/a b"));
        let mut p = Profiles::default();
        let mut f = fetched(&[1]);
        f.title = None;
        let id = p.add_subscription("https://panel.example.org/sub", &f);
        assert_eq!(p.subscription(&id).unwrap().name, "panel.example.org");
    }

    #[test]
    fn orphan_group_servers_are_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        let mut p = Profiles::default();
        let id = p.add_subscription("https://x.example/s", &fetched(&[1]));
        p.subscriptions.clear();
        p.save(&path).unwrap();
        let loaded = Profiles::load(&path).unwrap();
        assert!(loaded.servers.is_empty(), "servers of {id} should be gone");
    }

    fn sample(n: u16) -> Server {
        parse_link(&format!(
            "vless://2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c@h{n}.example:{n}?security=none#S{n}"
        ))
        .unwrap()
    }

    #[test]
    fn roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/profiles.json");
        let loaded = Profiles::load(&path).unwrap();
        assert_eq!(loaded, Profiles::default());
        assert_eq!(loaded.apply_mode, ApplyMode::SystemProxy);

        let mut p = Profiles::default();
        assert_eq!(p.add_servers([sample(1), sample(2), sample(1)]), 2);
        assert_eq!(p.servers.len(), 2);
        assert_eq!(p.selected, Some(p.servers[0].id.clone()));
        p.save(&path).unwrap();

        let again = Profiles::load(&path).unwrap();
        assert_eq!(again, p);
        assert!(!path.with_extension("tmp").exists());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn remove_moves_selection() {
        let mut p = Profiles::default();
        p.add_servers([sample(1), sample(2)]);
        let first = p.servers[0].id.clone();
        p.remove(&first).unwrap();
        assert_eq!(p.selected, Some(p.servers[0].id.clone()));
        p.remove(&p.servers[0].id.clone()).unwrap();
        assert_eq!(p.selected, None);
    }

    #[test]
    fn dangling_selection_is_cleared_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, r#"{"version":1,"servers":[],"selected":"nope"}"#).unwrap();
        let p = Profiles::load(&path).unwrap();
        assert_eq!(p.selected, None);
        assert_eq!(p.local_port, 2080);
        assert_eq!(p.dns, DEFAULT_DNS);
        assert!(!p.prerelease);
    }

    #[test]
    fn rename_validates() {
        let mut p = Profiles::default();
        p.add_servers([sample(1)]);
        let id = p.servers[0].id.clone();
        assert!(!p.rename(&id, "   "));
        assert!(p.rename(&id, "  New name "));
        assert_eq!(p.servers[0].server.name, "New name");
        assert!(!p.rename("nope", "x"));
    }
}
