//! Desktop system proxy: GNOME-family via GSettings and KDE via
//! `kioslaverc`. Previous settings are saved to `state.json` so they can be
//! restored on disconnect, on shutdown and on the next start after a crash.

use std::path::Path;

use gio::glib;
use gio::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const GNOME_SCHEMA: &str = "org.gnome.system.proxy";
const IGNORE_HOSTS: &[&str] = &["localhost", "127.0.0.0/8", "::1"];

/// Snapshot of the GNOME proxy settings we touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GnomeBackup {
    pub mode: String,
    pub use_same_proxy: bool,
    pub ignore_hosts: Vec<String>,
    pub http: (String, i32),
    pub https: (String, i32),
    pub socks: (String, i32),
    pub http_enabled: bool,
}

/// Snapshot of the `[Proxy Settings]` group of `kioslaverc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KdeBackup {
    pub proxy_type: Option<String>,
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    pub socks_proxy: Option<String>,
    pub no_proxy_for: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProxyBackup {
    pub gnome: Option<GnomeBackup>,
    pub kde: Option<KdeBackup>,
}

/// Persistent runtime state (`~/.local/share/obscure/state.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct State {
    /// Set while the system proxy is applied; `None` when clean.
    pub proxy_backup: Option<ProxyBackup>,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_private(path, &serde_json::to_vec_pretty(self)?)
    }
}

/// Applies and restores the system proxy, persisting the backup in `state`.
#[derive(Debug, Clone)]
pub struct SysProxy {
    state_file: std::path::PathBuf,
    kioslaverc: std::path::PathBuf,
}

impl SysProxy {
    pub fn new(state_file: std::path::PathBuf) -> Self {
        let kioslaverc = crate::paths::config_dir()
            .parent()
            .map(|p| p.join("kioslaverc"))
            .unwrap_or_else(|| std::path::PathBuf::from("kioslaverc"));
        Self {
            state_file,
            kioslaverc,
        }
    }

    /// `true` if a previous session left the system proxy applied.
    pub fn is_dirty(&self) -> bool {
        State::load(&self.state_file)
            .map(|s| s.proxy_backup.is_some())
            .unwrap_or(false)
    }

    /// Points the desktop at `127.0.0.1:port` (HTTP, HTTPS and SOCKS).
    /// Writes both GNOME and KDE settings when available. Idempotent: if a
    /// backup already exists it is kept, so re-applying never overwrites the
    /// user's real settings with ours.
    pub async fn apply(&self, port: u16) -> Result<()> {
        let mut state = State::load(&self.state_file)?;
        let mut backup = state.proxy_backup.take().unwrap_or_default();
        let mut touched = false;

        if let Some(gnome) = gnome_settings() {
            if backup.gnome.is_none() {
                backup.gnome = Some(gnome_read(&gnome));
            }
            gnome_write(&gnome, port);
            touched = true;
        }

        if kde_available() {
            if backup.kde.is_none() {
                backup.kde = Some(kde_read(&self.kioslaverc));
            }
            kde_write(port).await?;
            touched = true;
        }

        if !touched {
            return Err(Error::SysProxy(
                "no supported desktop found (GNOME GSettings or KDE kioslaverc)".into(),
            ));
        }
        state.proxy_backup = Some(backup);
        state.save(&self.state_file)
    }

    /// Restores whatever `apply` changed and marks the state clean.
    pub async fn restore(&self) -> Result<()> {
        let mut state = State::load(&self.state_file)?;
        let Some(backup) = state.proxy_backup.take() else {
            return Ok(());
        };
        if let (Some(b), Some(gnome)) = (&backup.gnome, gnome_settings()) {
            gnome_restore(&gnome, b);
        }
        if let Some(b) = &backup.kde
            && kde_available()
        {
            kde_restore(b).await?;
        }
        state.save(&self.state_file)
    }
}

// ---------------------------------------------------------------------------
// GNOME

struct GnomeSettings {
    root: gio::Settings,
    http: gio::Settings,
    https: gio::Settings,
    socks: gio::Settings,
}

fn gnome_settings() -> Option<GnomeSettings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(GNOME_SCHEMA, true)?;
    Some(GnomeSettings {
        root: gio::Settings::new(GNOME_SCHEMA),
        http: gio::Settings::new(&format!("{GNOME_SCHEMA}.http")),
        https: gio::Settings::new(&format!("{GNOME_SCHEMA}.https")),
        socks: gio::Settings::new(&format!("{GNOME_SCHEMA}.socks")),
    })
}

fn host_port(s: &gio::Settings) -> (String, i32) {
    (s.string("host").to_string(), s.int("port"))
}

fn gnome_read(g: &GnomeSettings) -> GnomeBackup {
    GnomeBackup {
        mode: g.root.string("mode").to_string(),
        use_same_proxy: g.root.boolean("use-same-proxy"),
        ignore_hosts: g
            .root
            .strv("ignore-hosts")
            .iter()
            .map(|s| s.to_string())
            .collect(),
        http: host_port(&g.http),
        https: host_port(&g.https),
        socks: host_port(&g.socks),
        http_enabled: g.http.boolean("enabled"),
    }
}

/// Logs a failed GSettings write instead of hiding it; a read-only key
/// (locked-down schema) would otherwise look like success.
fn check(result: std::result::Result<(), glib::BoolError>, key: &str) {
    if let Err(e) = result {
        tracing::warn!("gsettings: cannot set {key}: {e}");
    }
}

fn gnome_write(g: &GnomeSettings, port: u16) {
    let port = i32::from(port);
    for s in [&g.http, &g.https, &g.socks] {
        check(s.set_string("host", "127.0.0.1"), "host");
        check(s.set_int("port", port), "port");
    }
    check(g.http.set_boolean("enabled", true), "http.enabled");
    check(g.root.set_boolean("use-same-proxy", true), "use-same-proxy");
    check(
        g.root.set_strv("ignore-hosts", IGNORE_HOSTS),
        "ignore-hosts",
    );
    check(g.root.set_string("mode", "manual"), "mode");
    gio::Settings::sync();
}

fn gnome_restore(g: &GnomeSettings, b: &GnomeBackup) {
    // Mode first: switching away from manual is what matters most.
    let _ = g.root.set_string("mode", &b.mode);
    let _ = g.root.set_boolean("use-same-proxy", b.use_same_proxy);
    let hosts: Vec<&str> = b.ignore_hosts.iter().map(String::as_str).collect();
    let _ = g.root.set_strv("ignore-hosts", hosts.as_slice());
    for (s, (host, port)) in [
        (&g.http, &b.http),
        (&g.https, &b.https),
        (&g.socks, &b.socks),
    ] {
        let _ = s.set_string("host", host);
        let _ = s.set_int("port", *port);
    }
    let _ = g.http.set_boolean("enabled", b.http_enabled);
    gio::Settings::sync();
}

// ---------------------------------------------------------------------------
// KDE

const KDE_GROUP: &str = "Proxy Settings";

fn kwriteconfig() -> Option<&'static str> {
    ["kwriteconfig6", "kwriteconfig5"]
        .into_iter()
        .find(|bin| which(bin))
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn kde_available() -> bool {
    kwriteconfig().is_some()
}

/// Reads the current `[Proxy Settings]` keys straight from the file (no
/// external process needed for reading).
fn kde_read(kioslaverc: &Path) -> KdeBackup {
    let Ok(text) = std::fs::read_to_string(kioslaverc) else {
        return KdeBackup::default();
    };
    parse_kioslaverc(&text)
}

pub(crate) fn parse_kioslaverc(text: &str) -> KdeBackup {
    let mut b = KdeBackup::default();
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == format!("[{KDE_GROUP}]");
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = Some(v.trim().to_owned());
            match k.trim() {
                "ProxyType" => b.proxy_type = v,
                "httpProxy" => b.http_proxy = v,
                "httpsProxy" => b.https_proxy = v,
                "socksProxy" => b.socks_proxy = v,
                "NoProxyFor" => b.no_proxy_for = v,
                _ => {}
            }
        }
    }
    b
}

async fn kwrite(key: &str, value: Option<&str>) -> Result<()> {
    let bin = kwriteconfig().ok_or_else(|| Error::SysProxy("kwriteconfig not found".into()))?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["--file", "kioslaverc", "--group", KDE_GROUP, "--key", key]);
    match value {
        Some(v) => {
            cmd.arg(v);
        }
        None => {
            cmd.arg("--delete");
        }
    }
    let status = cmd.status().await.map_err(|e| Error::io(bin, e))?;
    if !status.success() {
        return Err(Error::SysProxy(format!("{bin} failed for {key}")));
    }
    Ok(())
}

async fn kde_write(port: u16) -> Result<()> {
    kwrite("ProxyType", Some("1")).await?;
    kwrite("httpProxy", Some(&format!("http://127.0.0.1 {port}"))).await?;
    kwrite("httpsProxy", Some(&format!("http://127.0.0.1 {port}"))).await?;
    kwrite("socksProxy", Some(&format!("socks://127.0.0.1 {port}"))).await?;
    kwrite("NoProxyFor", Some(&IGNORE_HOSTS.join(","))).await?;
    kde_notify().await
}

async fn kde_restore(b: &KdeBackup) -> Result<()> {
    kwrite("ProxyType", Some(b.proxy_type.as_deref().unwrap_or("0"))).await?;
    kwrite("httpProxy", b.http_proxy.as_deref()).await?;
    kwrite("httpsProxy", b.https_proxy.as_deref()).await?;
    kwrite("socksProxy", b.socks_proxy.as_deref()).await?;
    kwrite("NoProxyFor", b.no_proxy_for.as_deref()).await?;
    kde_notify().await
}

/// Tells running KDE apps to re-read `kioslaverc`.
async fn kde_notify() -> Result<()> {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("no session bus, KDE apps will pick the proxy up later: {e}");
            return Ok(());
        }
    };
    conn.emit_signal(
        None::<zbus::names::BusName>,
        "/KIO/Scheduler",
        "org.kde.KIO.Scheduler",
        "reparseSlaveConfiguration",
        &("",),
    )
    .await?;
    Ok(())
}

/// Shell snippet for terminals (`export http_proxy=…`).
pub fn env_snippet(port: u16) -> String {
    format!(
        "export http_proxy=http://127.0.0.1:{port}\nexport https_proxy=http://127.0.0.1:{port}\nexport all_proxy=socks5://127.0.0.1:{port}\nexport no_proxy={}\n",
        IGNORE_HOSTS.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kioslaverc_group_only() {
        let text = "[Other]\nProxyType=9\n\n[Proxy Settings]\nProxyType=1\nhttpProxy=http://127.0.0.1 2080\nNoProxyFor=localhost\n[Next]\nsocksProxy=nope\n";
        let b = parse_kioslaverc(text);
        assert_eq!(b.proxy_type.as_deref(), Some("1"));
        assert_eq!(b.http_proxy.as_deref(), Some("http://127.0.0.1 2080"));
        assert_eq!(b.no_proxy_for.as_deref(), Some("localhost"));
        assert_eq!(b.socks_proxy, None);
    }

    #[test]
    fn state_roundtrip_and_dirty_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let sp = SysProxy::new(path.clone());
        assert!(!sp.is_dirty());
        let st = State {
            proxy_backup: Some(ProxyBackup {
                gnome: None,
                kde: Some(KdeBackup::default()),
            }),
        };
        st.save(&path).unwrap();
        assert!(sp.is_dirty());
        assert_eq!(State::load(&path).unwrap(), st);
    }

    #[test]
    fn env_snippet_mentions_port() {
        let s = env_snippet(2080);
        assert!(s.contains("http://127.0.0.1:2080"));
        assert!(s.contains("socks5://127.0.0.1:2080"));
    }
}
