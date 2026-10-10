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
pub mod tun;

pub use error::{Error, Result};

/// Application version. Single source of truth: meson reads it from here
/// and the release tooling bumps it automatically (semantic version).
pub const APP_VERSION: &str = "0.1.0";

/// Default local port of the mixed (HTTP + SOCKS) inbound.
pub const DEFAULT_LOCAL_PORT: u16 = 2080;

/// Returns the User-Agent sent when fetching subscriptions and core releases.
pub fn user_agent(version: &str) -> String {
    format!("Obscure/{version} (Linux)")
}

/// Re-exported so the GUI does not need its own `reqwest` dependency.
pub type HttpClient = reqwest::Client;

/// HTTP client with Obscure's User-Agent, a sane timeout and a strict
/// redirect policy (see [`check_redirect`]).
pub fn http_client(user_agent: &str) -> HttpClient {
    init_crypto();
    reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(30))
        .redirect(redirect_policy())
        .build()
        .expect("reqwest client")
}

/// Most redirects followed for a single request.
pub const MAX_REDIRECTS: usize = 5;

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let previous = attempt.previous();
        if let Err(reason) = check_redirect(previous.last(), attempt.url(), previous.len()) {
            return attempt.error(reason);
        }
        subscription::record_redirect(attempt.status());
        attempt.follow()
    })
}

/// Decides whether a redirect from `from` to `to` may be followed, where
/// `seen` is the number of URLs requested so far (the first one included).
/// Refuses chains longer than [`MAX_REDIRECTS`] hops, non-HTTP targets and
/// HTTPS → HTTP downgrades, which would send subscription tokens in clear.
pub fn check_redirect(
    from: Option<&url::Url>,
    to: &url::Url,
    seen: usize,
) -> std::result::Result<(), &'static str> {
    if seen > MAX_REDIRECTS {
        return Err("too many redirects");
    }
    if !matches!(to.scheme(), "http" | "https") {
        return Err("redirect to a non-HTTP address");
    }
    if from.is_some_and(|f| f.scheme() == "https") && to.scheme() != "https" {
        return Err("redirect from HTTPS to plain HTTP");
    }
    Ok(())
}

/// Installs the rustls crypto provider. Safe to call more than once.
pub fn init_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_rules() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        let https = u("https://a.example/sub?token=1");
        let http = u("http://a.example/sub");
        assert!(check_redirect(Some(&https), &u("https://b.example/"), 1).is_ok());
        assert!(check_redirect(Some(&http), &https, 1).is_ok());
        assert!(check_redirect(Some(&http), &u("http://b.example/"), 1).is_ok());
        assert!(check_redirect(Some(&https), &http, 1).is_err());
        assert!(check_redirect(Some(&http), &u("ftp://b.example/x"), 1).is_err());
        assert!(check_redirect(Some(&https), &https, MAX_REDIRECTS).is_ok());
        assert!(check_redirect(Some(&https), &https, MAX_REDIRECTS + 1).is_err());
    }

    #[test]
    fn user_agent_has_expected_shape() {
        assert_eq!(user_agent("0.1.0"), "Obscure/0.1.0 (Linux)");
    }
}
