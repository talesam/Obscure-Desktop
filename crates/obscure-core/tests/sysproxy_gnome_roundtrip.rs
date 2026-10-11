//! GNOME-only desktop (in-memory GSettings, no kwriteconfig on PATH):
//! `apply` followed by `restore` must bring back every original value
//! exactly, even when the run starts with a corrupted `state.json`. One
//! test per binary: it sets process env.

use gio::prelude::*;
use obscure_core::sysproxy::SysProxy;

const SCHEMA: &str = "org.gnome.system.proxy";

#[derive(Debug, PartialEq)]
struct Snapshot {
    mode: String,
    use_same_proxy: bool,
    ignore_hosts: Vec<String>,
    autoconfig_url: String,
    hosts: Vec<(String, i32)>,
    http_enabled: bool,
}

fn snapshot() -> Snapshot {
    let root = gio::Settings::new(SCHEMA);
    let sub = |s: &str| gio::Settings::new(&format!("{SCHEMA}.{s}"));
    Snapshot {
        mode: root.string("mode").into(),
        use_same_proxy: root.boolean("use-same-proxy"),
        ignore_hosts: root
            .strv("ignore-hosts")
            .iter()
            .map(|s| s.to_string())
            .collect(),
        autoconfig_url: root.string("autoconfig-url").into(),
        hosts: ["http", "https", "socks"]
            .iter()
            .map(|s| {
                let g = sub(s);
                (g.string("host").into(), g.int("port"))
            })
            .collect(),
        http_enabled: sub("http").boolean("enabled"),
    }
}

#[tokio::test]
async fn apply_restore_keeps_every_gnome_value() {
    let has_schema = std::path::Path::new("/usr/share/glib-2.0/schemas/gschemas.compiled").exists()
        && std::path::Path::new("/usr/share/glib-2.0/schemas/org.gnome.system.proxy.gschema.xml")
            .exists();
    if !has_schema {
        eprintln!("org.gnome.system.proxy schema not installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A non-executable kwriteconfig6 must not count as KDE support.
    std::fs::write(bin.join("kwriteconfig6"), b"#!/bin/sh\nexit 1\n").unwrap();
    // SAFETY: single test per binary, set before any thread reads env.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_CONFIG_HOME", dir.path().join("config"));
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("PATH", &bin);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
        std::env::set_var("GSETTINGS_BACKEND", "memory");
    }

    // Unusual original values, so that a default would not pass by luck.
    {
        let root = gio::Settings::new(SCHEMA);
        root.set_string("mode", "auto").unwrap();
        root.set_boolean("use-same-proxy", false).unwrap();
        root.set_strv("ignore-hosts", ["corp.example", "10.0.0.0/8", "*.ç.local"])
            .unwrap();
        root.set_string("autoconfig-url", "http://wpad/wpad.dat")
            .unwrap();
        for (s, host, port) in [
            ("http", "proxy.corp", 3128),
            ("https", "secure.corp", 8443),
            ("socks", "socks.corp", 1080),
        ] {
            let g = gio::Settings::new(&format!("{SCHEMA}.{s}"));
            g.set_string("host", host).unwrap();
            g.set_int("port", port).unwrap();
        }
        gio::Settings::new(&format!("{SCHEMA}.http"))
            .set_boolean("enabled", false)
            .unwrap();
        gio::Settings::sync();
    }
    let before = snapshot();

    let state = dir.path().join("data/obscure/state.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(&state, b"\x00not json").unwrap();
    let sp = SysProxy::new(state.clone());
    assert!(sp.is_dirty(), "corrupt state counts as dirty");

    sp.apply(2080).await.expect("apply despite corrupt state");
    let corrupt = std::fs::read_dir(state.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("state.json.corrupt-")
        })
        .count();
    assert_eq!(corrupt, 1, "corrupt state moved aside");
    assert!(sp.is_dirty());

    let applied = snapshot();
    assert_eq!(applied.mode, "manual");
    assert!(applied.use_same_proxy);
    assert!(applied.http_enabled);
    for (host, port) in &applied.hosts {
        assert_eq!((host.as_str(), *port), ("127.0.0.1", 2080));
    }

    // Applying again must not overwrite the saved originals with ours.
    sp.apply(2081).await.unwrap();

    sp.restore().await.expect("restore");
    assert!(!sp.is_dirty());
    assert_eq!(snapshot(), before);

    // Restoring twice is a no-op.
    sp.restore().await.unwrap();
    assert_eq!(snapshot(), before);
}
