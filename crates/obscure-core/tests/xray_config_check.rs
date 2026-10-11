//! Validates generated configurations with the real Xray (`xray run -test`).
//!
//! Skipped unless `OBSCURE_TEST_XRAY` points to a directory holding `xray`,
//! `geoip.dat` and `geosite.dat` (e.g. `~/.local/share/obscure/core`).
//! Tunnel mode is left out: `-test` would create the TUN device.
//!
//! Run with: `cargo test -p obscure-core --test xray_config_check -- --nocapture`

mod corpus;

use std::path::PathBuf;
use std::process::Command;

use obscure_core::config::{ConfigOptions, build};
use obscure_core::links::parse_link;
use obscure_core::profile::{RoutePreset, RouteRule, RuleTarget};

/// Warnings about the server's own protocol or transport (Xray 26 nudges
/// everyone towards VLESS + XHTTP) say nothing about our config and cannot
/// be fixed on the client; anything else (deprecated keys…) is a failure.
fn is_inherent_warning(line: &str) -> bool {
    const FEATURES: [&str; 6] = [
        "The feature WebSocket transport",
        "The feature gRPC transport",
        "The feature HTTPUpgrade transport",
        "The feature VMess ",
        "The feature Trojan ",
        "The feature Shadowsocks ",
    ];
    (FEATURES.iter().any(|f| line.contains(f)) && line.contains("is deprecated"))
        || line.contains("proxy/wireguard: Using kernel TUN")
}

#[test]
fn generated_configs_pass_xray_test() {
    let Ok(dir) = std::env::var("OBSCURE_TEST_XRAY") else {
        eprintln!("skipping: OBSCURE_TEST_XRAY not set");
        return;
    };
    let dir = PathBuf::from(dir);
    let xray = dir.join("xray");
    assert!(xray.exists(), "{} not found", xray.display());
    let tmp = tempfile::tempdir().unwrap();
    let rules = vec![
        RouteRule {
            pattern: "example.com".into(),
            target: RuleTarget::Direct,
        },
        RouteRule {
            pattern: "10.0.0.0/8".into(),
            target: RuleTarget::Block,
        },
        RouteRule {
            pattern: "geosite:google".into(),
            target: RuleTarget::Proxy,
        },
        RouteRule {
            pattern: "geoip:cn".into(),
            target: RuleTarget::Direct,
        },
    ];
    let domains = vec!["example.org".to_owned(), "geosite:netflix".to_owned()];
    let presets = [
        RoutePreset::All,
        RoutePreset::Smart,
        RoutePreset::OnlyListed,
    ];

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let mut checked = 0;
    for (i, link) in corpus::links().iter().enumerate() {
        let server = parse_link(link).unwrap_or_else(|e| panic!("{link}: {e}"));
        let preset = presets[i % presets.len()];
        let mut opts = ConfigOptions::new(&server);
        opts.route_preset = preset;
        opts.api_port = Some(10085);
        opts.custom_rules = &rules;
        opts.listed_domains = &domains;
        let cfg = build(&opts);
        let path = tmp.path().join(format!("config-{i}.json"));
        std::fs::write(&path, serde_json::to_vec_pretty(&cfg).unwrap()).unwrap();
        let out = Command::new(&xray)
            .args(["run", "-test", "-c"])
            .arg(&path)
            .env("XRAY_LOCATION_ASSET", &dir)
            .output()
            .expect("run xray");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        checked += 1;
        let label = format!(
            "#{i} {} ({} / {} / {}, {preset:?})",
            server.name,
            server.protocol.label(),
            server.transport.label(),
            server.security.label()
        );
        if !out.status.success() || !text.contains("Configuration OK") {
            failures.push(format!("{label}\n{text}"));
            continue;
        }
        for line in text.lines().filter(|l| l.contains("[Warning]")) {
            if is_inherent_warning(line) {
                eprintln!("info {label}: {line}");
            } else {
                warnings.push(format!("{label}: {line}"));
            }
        }
        eprintln!("ok   {label}");
    }
    for w in &warnings {
        eprintln!("WARN {w}");
    }
    for f in &failures {
        eprintln!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{} configs rejected", failures.len());
    assert!(warnings.is_empty(), "{} warnings", warnings.len());
    eprintln!("{checked} configurations accepted by Xray without config warnings");
}
