//! Manual check: downloads the geo files into a temp dir.
//! `cargo run -p obscure-core --example geo_update`
use obscure_core::core_manager::CoreManager;

#[tokio::main]
async fn main() {
    let dir = std::env::temp_dir().join("obscure-geo-test");
    let mgr = CoreManager::new(dir.clone(), &obscure_core::user_agent("dev"));
    println!("needs update: {}", mgr.geo_needs_update());
    mgr.update_geo().await.expect("geo update");
    println!("needs update after: {}", mgr.geo_needs_update());
    for f in ["geoip.dat", "geosite.dat"] {
        println!(
            "{f}: {} bytes",
            std::fs::metadata(dir.join(f)).unwrap().len()
        );
    }
}
