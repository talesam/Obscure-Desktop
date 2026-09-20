//! Core library of Obscure. Must never depend on GTK.

pub mod access;
pub mod config;
pub mod core_manager;
pub mod error;
pub mod geoip;
pub mod latency;
pub mod links;
pub mod paths;
pub mod profile;
pub mod stats;
pub mod subscription;
pub mod supervisor;
pub mod sysproxy;

pub use error::{Error, Result};

/// Default local port of the mixed (HTTP + SOCKS) inbound.
pub const DEFAULT_LOCAL_PORT: u16 = 2080;

/// Returns the User-Agent sent when fetching subscriptions and core releases.
pub fn user_agent(version: &str) -> String {
    format!("Obscure/{version} (Linux)")
}

/// Re-exported so the GUI does not need its own `reqwest` dependency.
pub type HttpClient = reqwest::Client;

/// HTTP client with Obscure's User-Agent and a sane timeout.
pub fn http_client(user_agent: &str) -> HttpClient {
    init_crypto();
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

/// Installs the rustls crypto provider. Safe to call more than once.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_expected_shape() {
        assert_eq!(user_agent("0.1.0"), "Obscure/0.1.0 (Linux)");
    }
}
