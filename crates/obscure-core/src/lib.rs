//! Core library of Obscure. Must never depend on GTK.
//!
//! Phase 1 will add: share-link parsing, profile persistence, Xray config
//! generation, core download/verification, process supervision, stats,
//! system proxy and subscriptions.

/// Default local port of the mixed (HTTP + SOCKS) inbound.
pub const DEFAULT_LOCAL_PORT: u16 = 2080;

/// Returns the User-Agent sent when fetching subscriptions and core releases.
pub fn user_agent(version: &str) -> String {
    format!("Obscure/{version} (Linux)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_has_expected_shape() {
        assert_eq!(user_agent("0.1.0"), "Obscure/0.1.0 (Linux)");
    }
}
