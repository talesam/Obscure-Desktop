//! KDE-only desktop with an odd `kioslaverc` (CRLF, duplicate keys, the
//! group split in two): `restore` must write back exactly the values that
//! were there before `apply`, and delete the keys that did not exist. A
//! fake `kwriteconfig6` records its calls. One test per binary: it sets
//! process env.

use obscure_core::sysproxy::SysProxy;

#[tokio::test]
async fn apply_restore_writes_back_original_kde_values() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("kwrite.log");
    let script = bin.join("kwriteconfig6");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s|' \"$a\" >> '{}'; done\necho >> '{}'\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("kioslaverc"),
        "[Proxy Settings]\r\nProxyType=4\r\nProxyType=2\r\nhttpProxy=http://old.corp 3128\r\n[Other]\r\nsocksProxy=not-this\r\n[Proxy Settings]\r\nNoProxyFor=a=b,ç.example\r\n",
    )
    .unwrap();
    // SAFETY: single test per binary, set before any thread reads env.
    unsafe {
        std::env::set_var("HOME", dir.path());
        std::env::set_var("XDG_CONFIG_HOME", &config);
        std::env::set_var("XDG_DATA_HOME", dir.path().join("data"));
        std::env::set_var("PATH", &bin);
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent");
        std::env::set_var("GSETTINGS_BACKEND", "memory");
        std::env::set_var("XDG_DATA_DIRS", dir.path().join("none"));
        std::env::set_var("GSETTINGS_SCHEMA_DIR", dir.path().join("none"));
    }
    let sp = SysProxy::new(dir.path().join("data/obscure/state.json"));

    sp.apply(2080).await.expect("apply");
    assert!(sp.is_dirty());
    let applied = std::fs::read_to_string(&log).unwrap();
    assert!(applied.contains("--key|ProxyType|1|"), "{applied}");
    std::fs::write(&log, "").unwrap();

    sp.restore().await.expect("restore");
    assert!(!sp.is_dirty());
    let calls: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| {
            l.trim_end_matches('|')
                .split("--key|")
                .nth(1)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            "ProxyType|2",
            "httpProxy|http://old.corp 3128",
            "httpsProxy|--delete",
            "socksProxy|--delete",
            "NoProxyFor|a=b,ç.example",
        ]
    );
}
