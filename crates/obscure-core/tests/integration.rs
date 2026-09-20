//! End-to-end check with a real Xray binary and a real server.
//!
//! Skipped unless both variables are set:
//!   OBSCURE_TEST_LINK  – a share link to a working server
//!   OBSCURE_TEST_XRAY  – directory containing `xray`, `geoip.dat`, `geosite.dat`
//!
//! Run with: `cargo test -p obscure-core --test integration -- --nocapture`

use std::path::PathBuf;
use std::time::Duration;

use obscure_core::config::{ConfigOptions, build};
use obscure_core::links::parse_link;
use obscure_core::supervisor::{Supervisor, write_config};

#[tokio::test]
async fn connect_and_fetch_through_proxy() {
    let (Ok(link), Ok(xray_dir)) = (
        std::env::var("OBSCURE_TEST_LINK"),
        std::env::var("OBSCURE_TEST_XRAY"),
    ) else {
        eprintln!("skipping: OBSCURE_TEST_LINK / OBSCURE_TEST_XRAY not set");
        return;
    };
    obscure_core::init_crypto();
    let xray_dir = PathBuf::from(xray_dir);
    let server = parse_link(&link).expect("link parses");
    eprintln!(
        "server: {} ({} / {} / {})",
        server.name,
        server.protocol.label(),
        server.transport.label(),
        server.security.label()
    );

    let port = 28080;
    let mut opts = ConfigOptions::new(&server);
    opts.local_port = port;
    opts.log_level = "info";
    let cfg = build(&opts);

    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("config.json");
    write_config(&cfg_path, &cfg).unwrap();

    let sup = Supervisor::new(xray_dir.join("xray"), xray_dir.clone());
    let mut core = sup.start(&cfg_path, port).await.expect("xray starts");
    eprintln!("xray pid {:?} listening on {port}", core.pid());

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let resp = client
        .get("https://www.gstatic.com/generate_204")
        .send()
        .await
        .expect("request through proxy");
    eprintln!("status through proxy: {}", resp.status());
    assert_eq!(resp.status().as_u16(), 204);

    // Also exercise the HTTP proxy side of the mixed inbound.
    let client_http = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).unwrap())
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let resp = client_http
        .get("https://www.gstatic.com/generate_204")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 204);

    while let Ok(ev) = core.events.try_recv() {
        eprintln!("xray: {ev:?}");
    }
    assert!(core.check_exit().is_none(), "xray still running");
    core.stop().await;
}
