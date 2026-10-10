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

    let (target, tags) = match tag_bracket(after) {
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

/// Byte index of the `[` that opens the `[inbound >> outbound]` tags: the
/// last one that starts a word and does not wrap an IPv6 address (so
/// `tcp:[::1]:443` or a bare `[2001:db8::1]:443` are left in the target).
fn tag_bracket(after: &str) -> Option<usize> {
    let mut end = after.len();
    while let Some(i) = after[..end].rfind('[') {
        let starts_word = after[..i]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let inner = after[i + 1..].split(']').next().unwrap_or_default();
        let addr = inner.split('%').next().unwrap_or_default();
        if starts_word && addr.parse::<std::net::Ipv6Addr>().is_err() {
            return Some(i);
        }
        end = i;
    }
    None
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
    fn ipv6_destinations() {
        let e = parse_access_line(
            "2026/09/20 00:13:32.5 from tcp:[::1]:5000 accepted tcp:[::1]:443 [mixed-in >> direct]",
        )
        .unwrap();
        assert_eq!(e.destination, "[::1]:443");
        assert_eq!(e.network.as_deref(), Some("tcp"));
        assert_eq!(
            (e.inbound.as_str(), e.outbound.as_str()),
            ("mixed-in", "direct")
        );

        // No tags: the address bracket must not be taken for them.
        let e = parse_access_line("2026/09/20 00:13:32.5 from 127.0.0.1:9 accepted tcp:[::1]:443")
            .unwrap();
        assert_eq!(e.destination, "[::1]:443");
        assert_eq!(e.inbound, "");
        let e = parse_access_line(
            "2026/09/20 00:13:32.5 from 127.0.0.1:9 accepted [2001:db8::1%eth0]:443",
        )
        .unwrap();
        assert_eq!(e.destination, "[2001:db8::1%eth0]:443");
        assert_eq!(e.network, None);
        let e = parse_access_line(
            "2026/09/20 00:13:32.5 from 127.0.0.1:9 accepted //[2606:4700::1111]:443 [tun-in -> proxy]",
        )
        .unwrap();
        assert_eq!(e.destination, "[2606:4700::1111]:443");
        assert_eq!(e.outbound, "proxy");
    }

    #[test]
    fn methods_missing_fields_and_unicode() {
        let e = parse_access_line(
            "2026/09/20 19:01:32.7 from 127.0.0.1:1 accepted POST http://example.com/x [mixed-in >> proxy]",
        )
        .unwrap();
        assert_eq!(e.method.as_deref(), Some("POST"));
        assert_eq!(e.destination, "http://example.com/x");

        // Missing time, missing tags, empty outbound, unclosed bracket.
        let e = parse_access_line(" from x accepted y").unwrap();
        assert_eq!((e.time.as_str(), e.destination.as_str()), ("", "y"));
        let e = parse_access_line("a b from x accepted y [in >> ]").unwrap();
        assert_eq!((e.inbound.as_str(), e.outbound.as_str()), ("in", ""));
        let e = parse_access_line("a b from x accepted y [in >> out").unwrap();
        assert_eq!(e.outbound, "out");
        let e = parse_access_line("a b from x accepted  [").unwrap();
        assert_eq!(e.destination, "");
        assert!(parse_access_line("from x accepted y").is_none());
        assert!(parse_access_line("a from x").is_none());

        let e = parse_access_line(
            "2026/09/20 19:01:32.7 from 127.0.0.1:1 accepted //xn--ç-ü.例え.jp:443 [entrée >> sortie🇧🇷]",
        )
        .unwrap();
        assert_eq!(e.destination, "xn--ç-ü.例え.jp:443");
        assert_eq!(
            (e.inbound.as_str(), e.outbound.as_str()),
            ("entrée", "sortie🇧🇷")
        );
    }

    /// Tiny xorshift PRNG: deterministic, no dependencies.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[test]
    fn never_panics_on_random_or_mutated_lines() {
        const SEEDS: &[&str] = &[
            "2026/09/20 19:01:32.534899 from 127.0.0.1:62888 accepted //www.gstatic.com:443 [mixed-in >> direct]",
            "2026/09/20 19:01:32.732348 from 127.0.0.1:62902 accepted http://ads.doubleclick.net/ [mixed-in -> block]",
            "2026/09/20 00:13:32.594935 from tcp:[::1]:11548 accepted tcp:[2001:db8::1]:443 [mixed-in >> proxy] email: u@x",
            "2026/09/20 00:13:32.5 from 127.0.0.1:9 rejected  proxy/socks: unknown Socks version: 71",
            "2026/09/20 00:13:32.5 from udp:1.2.3.4:5 accepted udp:8.8.8.8:53 [tun-in >> dns-out]",
        ];
        const PIECES: &[&str] = &[
            " from ",
            " accepted ",
            " rejected ",
            "[",
            "]",
            " >> ",
            " -> ",
            "tcp:",
            "udp:",
            "//",
            ":",
            ".",
            " ",
            "\t",
            "ü",
            "例",
            "🇧🇷",
            "\u{0}",
            "\u{FEFF}",
            "%",
            "::1",
            "GET ",
            "",
        ];
        let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
        let mut parsed = 0usize;
        for i in 0..40_000 {
            let line: String = if i % 4 == 0 {
                // Pure noise assembled from meaningful fragments.
                (0..rng.below(16))
                    .map(|_| PIECES[rng.below(PIECES.len())])
                    .collect()
            } else {
                // Mutate a real line at char boundaries.
                let mut chars: Vec<char> = SEEDS[rng.below(SEEDS.len())].chars().collect();
                for _ in 0..1 + rng.below(6) {
                    let pos = rng.below(chars.len() + 1);
                    match rng.below(4) {
                        0 if pos < chars.len() => {
                            chars.remove(pos);
                        }
                        1 => {
                            let piece = PIECES[rng.below(PIECES.len())];
                            for (k, c) in piece.chars().enumerate() {
                                chars.insert(pos + k, c);
                            }
                        }
                        2 if pos < chars.len() => {
                            chars[pos] =
                                char::from_u32(rng.next() as u32 % 0x11_0000).unwrap_or('\u{FFFD}');
                        }
                        _ => chars.truncate(pos),
                    }
                }
                chars.into_iter().collect()
            };
            if let Some(ev) = parse_access_line(&line) {
                parsed += 1;
                // Whatever comes out must survive the UI's serde path.
                let json = serde_json::to_string(&ev).unwrap();
                assert_eq!(serde_json::from_str::<AccessEvent>(&json).unwrap(), ev);
            }
        }
        assert!(
            parsed > 10_000,
            "mutations should mostly still parse: {parsed}"
        );
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
