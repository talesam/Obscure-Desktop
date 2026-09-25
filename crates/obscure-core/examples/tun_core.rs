//! Manual check: resolves and installs a tunnel-capable core into a temp dir.
//! `cargo run -p obscure-core --example tun_core`
use obscure_core::core_manager::{CoreManager, Progress, supports_tun_routing};

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("obscure-tun-core-test");
    let mgr = CoreManager::new(dir.clone(), &obscure_core::user_agent("dev"));
    let (version, replaced) = mgr
        .ensure_tun_capable(|p| {
            if let Progress::Downloading { received, total } = p
                && received % (8 << 20) < (1 << 16)
            {
                eprintln!("  {received} / {total:?}");
            }
        })
        .await
        .expect("ensure_tun_capable");
    println!(
        "version {version}, replaced {replaced}, supports {}",
        supports_tun_routing(&version)
    );
    let out = std::process::Command::new(dir.join("xray"))
        .arg("version")
        .output()
        .unwrap();
    println!(
        "{}",
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
    );
}
