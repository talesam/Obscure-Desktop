//! Parser for Xray access-log lines, so the GUI can show "what is going
//! where" as it happens. Enabled by `log.access = ""` in the config.
//!
//! Examples of lines Xray prints:
//! `2026/09/20 19:01:32.534899 from 127.0.0.1:62888 accepted //www.gstatic.com:443 [mixed-in >> direct]`
//! `2026/09/20 19:01:32.732348 from 127.0.0.1:62902 accepted http://ads.doubleclick.net/ [mixed-in -> block]`
//! `2026/09/20 00:13:32.594935 from tcp:127.0.0.1:11548 accepted tcp:www.gstatic.com:443 [mixed-in >> proxy]`

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Accepted,
    Rejected,
}

/// One connection as seen by Xray.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEvent {
    /// `HH:MM:SS` (local time as printed by Xray).
    pub time: String,
    /// `tcp` or `udp` when known.
    pub network: Option<String>,
    /// `host:port`, or the URL for plain HTTP requests.
    pub destination: String,
    pub inbound: String,
    /// Outbound tag: `proxy`, `direct`, `block`…
    pub outbound: String,
    pub verdict: Verdict,
    /// Only present for HTTP requests: `GET`, `POST`…
    pub method: Option<String>,
}

/// Parses a line; returns `None` for anything that is not an access record.
pub fn parse_access_line(line: &str) -> Option<AccessEvent> {
    let (prefix, rest) = line.split_once(" from ")?;
    let time = prefix
        .split_whitespace()
        .nth(1)
        .and_then(|t| t.split('.').next())
        .unwrap_or_default()
        .to_owned();

    let (verdict, after) = if let Some((_, r)) = rest.split_once(" accepted ") {
        (Verdict::Accepted, r)
    } else if let Some((_, r)) = rest.split_once(" rejected ") {
        (Verdict::Rejected, r)
    } else {
        return None;
    };

    let (target, tags) = match after.rfind('[') {
        Some(i) => {
            let tail = &after[i + 1..];
            (
                after[..i].trim(),
                tail.split(']').next().unwrap_or_default(),
            )
        }
        None => (after.trim(), ""),
    };
    // Rejected lines carry the reason instead of tags; tags may be empty.
    let (inbound, outbound) = split_tags(tags);

    let (method, target) = match target.split_once(' ') {
        Some((m, t)) if m.chars().all(|c| c.is_ascii_uppercase()) && m.len() <= 7 => {
            (Some(m.to_owned()), t.trim())
        }
        _ => (None, target),
    };

    let (network, destination) = if let Some(d) = target.strip_prefix("tcp:") {
        (Some("tcp".to_owned()), d.to_owned())
    } else if let Some(d) = target.strip_prefix("udp:") {
        (Some("udp".to_owned()), d.to_owned())
    } else if let Some(d) = target.strip_prefix("//") {
        (Some("tcp".to_owned()), d.to_owned())
    } else {
        (None, target.to_owned())
    };

    Some(AccessEvent {
        time,
        network,
        destination,
        inbound,
        outbound,
        verdict,
        method,
    })
}

fn split_tags(tags: &str) -> (String, String) {
    for sep in [" >> ", " -> "] {
        if let Some((i, o)) = tags.split_once(sep) {
            let outbound = o.split_whitespace().next().unwrap_or_default();
            return (i.trim().to_owned(), outbound.to_owned());
        }
    }
    (tags.trim().to_owned(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_connect_via_direct() {
        let e = parse_access_line(
            "2026/09/20 19:01:32.534899 from 127.0.0.1:62888 accepted //www.gstatic.com:443 [mixed-in >> direct]",
        )
        .unwrap();
        assert_eq!(e.time, "19:01:32");
        assert_eq!(e.destination, "www.gstatic.com:443");
        assert_eq!(e.network.as_deref(), Some("tcp"));
        assert_eq!(e.inbound, "mixed-in");
        assert_eq!(e.outbound, "direct");
        assert_eq!(e.verdict, Verdict::Accepted);
        assert_eq!(e.method, None);
    }

    #[test]
    fn plain_http_blocked() {
        let e = parse_access_line(
            "2026/09/20 19:01:32.732348 from 127.0.0.1:62902 accepted http://ads.doubleclick.net/ [mixed-in -> block]",
        )
        .unwrap();
        assert_eq!(e.destination, "http://ads.doubleclick.net/");
        assert_eq!(e.outbound, "block");
    }

    #[test]
    fn socks_with_network_prefix_and_email() {
        let e = parse_access_line(
            "2026/09/20 00:13:32.594935 from tcp:127.0.0.1:11548 accepted tcp:www.gstatic.com:443 [mixed-in >> proxy] email: u@x",
        )
        .unwrap();
        assert_eq!(e.destination, "www.gstatic.com:443");
        assert_eq!(e.outbound, "proxy");
        let u = parse_access_line("2026/09/20 00:13:32.5 from udp:127.0.0.1:5 accepted udp:1.1.1.1:53 [mixed-in >> proxy]").unwrap();
        assert_eq!(u.network.as_deref(), Some("udp"));
    }

    #[test]
    fn rejected_and_noise() {
        let r = parse_access_line("2026/09/20 00:13:32.5 from 127.0.0.1:9 rejected  proxy/socks: unknown Socks version: 71").unwrap();
        assert_eq!(r.verdict, Verdict::Rejected);
        assert!(
            parse_access_line("2026/09/20 00:13:32.3 [Warning] core: Xray 26.3.27 started")
                .is_none()
        );
        assert!(parse_access_line("").is_none());
    }
}
