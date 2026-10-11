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

    /// Like [`State::load`], but an unparsable file does not block the
    /// system proxy forever: it is moved aside to
    /// `state.json.corrupt-<unix time>` (kept for inspection) and an empty
    /// state is returned. Read errors other than "not found" still fail.
    pub fn load_or_quarantine(path: &Path) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(Error::io(path, e)),
        };
        match serde_json::from_slice(&bytes) {
            Ok(s) => Ok(s),
            Err(e) => {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let mut aside = path.as_os_str().to_owned();
                aside.push(format!(".corrupt-{ts}"));
                let aside = std::path::PathBuf::from(aside);
                match std::fs::rename(path, &aside) {
                    Ok(()) => tracing::warn!(
                        "unreadable {} ({e}); moved to {} and starting clean",
                        path.display(),
                        aside.display()
                    ),
                    Err(re) => {
                        tracing::warn!(
                            "unreadable {} ({e}); cannot move it aside: {re}",
                            path.display()
                        );
                        // Do not keep failing on it: replace it outright.
                        Self::default().save(path)?;
                    }
                }
                Ok(Self::default())
            }
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
    ///
    /// A state file that exists but cannot be parsed counts as dirty
    /// ("unknown"): `restore` then moves it aside instead of ignoring it.
    pub fn is_dirty(&self) -> bool {
        match State::load(&self.state_file) {
            Ok(s) => s.proxy_backup.is_some(),
            Err(Error::Io { .. }) => false,
            Err(_) => true,
        }
    }

    /// Points the desktop at `127.0.0.1:port` (HTTP, HTTPS and SOCKS).
    /// Writes both GNOME and KDE settings when available. Idempotent: if a
    /// backup already exists it is kept, so re-applying never overwrites the
    /// user's real settings with ours.
    pub async fn apply(&self, port: u16) -> Result<()> {
        let mut state = State::load_or_quarantine(&self.state_file)?;
        let mut backup = state.proxy_backup.take().unwrap_or_default();
        let kde = kde_available();
        // GSettings reads/writes (and `sync`) block; keep them off the async
        // workers. `gio::Settings` is not `Send`, so it lives entirely
        // inside the blocking closure.
        let need_gnome_backup = backup.gnome.is_none();
        let (has_gnome, gnome_backup) = gnome_blocking(move || match gnome_settings() {
            Some(g) => (true, need_gnome_backup.then(|| gnome_read(&g))),
            None => (false, None),
        })
        .await?;
        if gnome_backup.is_some() {
            backup.gnome = gnome_backup;
        }
        if !has_gnome && !kde {
            return Err(Error::SysProxy(
                "no supported desktop found (GNOME GSettings or KDE kioslaverc)".into(),
            ));
        }
        if kde && backup.kde.is_none() {
            backup.kde = Some(kde_read(&self.kioslaverc));
        }
        // Persist the backup before touching anything: if a write below
        // fails, or the app dies halfway, `restore` still has what it needs.
        state.proxy_backup = Some(backup);
        state.save(&self.state_file)?;

        if has_gnome {
            gnome_blocking(move || {
                if let Some(g) = gnome_settings() {
                    gnome_write(&g, port);
                }
            })
            .await?;
        }
        if kde && let Err(e) = kde_write(port).await {
            if has_gnome {
                // GNOME-family settings are applied; KDE apps just miss out.
                tracing::warn!("KDE proxy settings not applied: {e}");
            } else {
                if let Err(re) = self.restore().await {
                    tracing::warn!("cannot roll back the system proxy: {re}");
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Restores whatever `apply` changed and marks the state clean.
    pub async fn restore(&self) -> Result<()> {
        let mut state = State::load_or_quarantine(&self.state_file)?;
        let Some(backup) = state.proxy_backup.take() else {
            return Ok(());
        };
        if let Some(b) = backup.gnome.clone() {
            gnome_blocking(move || {
                if let Some(g) = gnome_settings() {
                    gnome_restore(&g, &b);
                }
            })
            .await?;
        }
        if let Some(b) = &backup.kde
            && kde_available()
        {
            kde_restore(b).await?;
        }
        state.save(&self.state_file)
    }
}

/// Runs GSettings work on the blocking pool.
async fn gnome_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::SysProxy(format!("gsettings task failed: {e}")))
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

/// `true` if an executable regular file named `bin` is on `PATH`.
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| is_executable(&d.join(bin))))
        .unwrap_or(false)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
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
    fn parses_odd_kioslaverc_files() {
        // No group at all.
        assert_eq!(
            parse_kioslaverc("ProxyType=1\nhttpProxy=x\n"),
            KdeBackup::default()
        );
        // CRLF line endings, spaces around '=', duplicate keys (last wins,
        // like KConfig) and the group appearing twice.
        let text = "[Proxy Settings]\r\nProxyType = 1\r\nProxyType=2\r\n[Other]\r\nhttpProxy=no\r\n[Proxy Settings]\r\nhttpProxy=http://h 8080\r\nsocksProxy=\r\n";
        let b = parse_kioslaverc(text);
        assert_eq!(b.proxy_type.as_deref(), Some("2"));
        assert_eq!(b.http_proxy.as_deref(), Some("http://h 8080"));
        assert_eq!(b.socks_proxy.as_deref(), Some(""));
        assert_eq!(b.https_proxy, None);
        // Values may contain '='; garbage lines are skipped; no panics on
        // unicode or a truncated header.
        let b = parse_kioslaverc(
            "[Proxy Settings]\nNoProxyFor=a=b,ç.example\njunk line\n=orphan\n[Proxy Sett",
        );
        assert_eq!(b.no_proxy_for.as_deref(), Some("a=b,ç.example"));
        assert_eq!(parse_kioslaverc(""), KdeBackup::default());
    }

    #[test]
    fn which_requires_the_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("kwriteconfig6");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&f));
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&f));
        assert!(!is_executable(dir.path()), "directories do not count");
        assert!(!is_executable(&dir.path().join("missing")));
    }

    fn corrupt_files(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("state.json.corrupt-"))
            .collect()
    }

    #[tokio::test]
    async fn corrupt_state_is_dirty_then_quarantined_by_restore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, b"{\"proxy_backup\": {\"gno").unwrap();
        let sp = SysProxy::new(path.clone());
        assert!(State::load(&path).is_err());
        assert!(sp.is_dirty(), "unknown state must be looked at");
        // Nothing known to restore, but the broken file no longer blocks.
        sp.restore().await.unwrap();
        assert!(!path.exists());
        assert_eq!(corrupt_files(dir.path()).len(), 1);
        assert!(!sp.is_dirty());
        assert_eq!(State::load_or_quarantine(&path).unwrap(), State::default());
    }

    #[test]
    fn quarantine_keeps_good_state_and_moves_bad_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(State::load_or_quarantine(&path).unwrap(), State::default());
        let st = State {
            proxy_backup: Some(ProxyBackup::default()),
        };
        st.save(&path).unwrap();
        assert_eq!(State::load_or_quarantine(&path).unwrap(), st);
        assert!(corrupt_files(dir.path()).is_empty());
        for junk in [
            &b""[..],
            b"null",
            b"[]",
            b"\xff\xfe",
            b"{\"proxy_backup\": 5}",
        ] {
            std::fs::write(&path, junk).unwrap();
            assert_eq!(State::load_or_quarantine(&path).unwrap(), State::default());
            assert!(!path.exists());
        }
        assert!(!corrupt_files(dir.path()).is_empty());
    }

    #[test]
    fn env_snippet_mentions_port() {
        let s = env_snippet(2080);
        assert!(s.contains("http://127.0.0.1:2080"));
        assert!(s.contains("socks5://127.0.0.1:2080"));
    }
}
