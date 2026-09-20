//! Persistent user data: servers, selection and connection preferences.
//! Stored as JSON in `~/.config/obscure/profiles.json` with a `version`
//! field for future migrations.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::links::Server;

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
    #[serde(flatten)]
    pub server: Server,
}

impl ServerEntry {
    pub fn new(server: Server) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            server,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profiles {
    pub version: u32,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub apply_mode: ApplyMode,
    #[serde(default)]
    pub route_preset: RoutePreset,
    #[serde(default = "default_local_port")]
    pub local_port: u16,
    /// Domains routed through the proxy in `OnlyListed` mode.
    #[serde(default)]
    pub listed_domains: Vec<String>,
}

fn default_local_port() -> u16 {
    crate::DEFAULT_LOCAL_PORT
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            servers: Vec::new(),
            selected: None,
            apply_mode: ApplyMode::default(),
            route_preset: RoutePreset::default(),
            local_port: default_local_port(),
            listed_domains: Vec::new(),
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

    /// Atomically writes to `path` (write temp + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let tmp: PathBuf = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, json).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))?;
        Ok(())
    }

    fn migrate(&mut self) {
        // Future versions bump CURRENT_VERSION and transform fields here.
        self.version = CURRENT_VERSION;
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

    pub fn remove(&mut self, id: &str) -> Option<ServerEntry> {
        let pos = self.servers.iter().position(|e| e.id == id)?;
        let removed = self.servers.remove(pos);
        if self.selected.as_deref() == Some(id) {
            self.selected = self.servers.first().map(|e| e.id.clone());
        }
        Some(removed)
    }

    pub fn get(&self, id: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|e| e.id == id)
    }

    pub fn selected_entry(&self) -> Option<&ServerEntry> {
        self.selected.as_deref().and_then(|id| self.get(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::parse_link;

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
        assert!(!path.with_extension("json.tmp").exists());
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
    }
}
