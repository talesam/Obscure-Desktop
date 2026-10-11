//! Round trip over a corpus of share links, and a dependency-free fuzz run:
//! random and mutated input must never panic the parser or the config
//! builder, and every server that parses must build a serialisable config
//! and export to a link that parses back to the same server.

mod corpus;

use std::panic::{AssertUnwindSafe, catch_unwind};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use obscure_core::config::{ConfigOptions, build};
use obscure_core::links::{Server, looks_like_link, parse_link, parse_many, to_link, try_to_link};
use obscure_core::profile::{RoutePreset, RouteRule, RuleTarget};

#[test]
fn corpus_round_trips_exactly() {
    let links = corpus::links();
    for link in &links {
        let server = parse_link(link).unwrap_or_else(|e| panic!("{link}: {e}"));
        let exported = try_to_link(&server).expect("consistent server exports");
        let again =
            parse_link(&exported).unwrap_or_else(|e| panic!("re-import of {exported}: {e}"));
        assert_eq!(again, server, "\nlink: {link}\nexported: {exported}");
        // Exporting is stable: a second round gives the same link.
        assert_eq!(to_link(&again), exported);
    }
    let (ok, failed) = parse_many(&links.join("\n"));
    assert_eq!(ok.len(), links.len());
    assert!(failed.is_empty(), "{failed:?}");
}

/// xorshift64*: small, deterministic, good enough to drive mutations.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const SCHEMES: &[&str] = &[
    "vless://",
    "vmess://",
    "trojan://",
    "ss://",
    "wireguard://",
    "wg://",
    "socks://",
    "socks5://",
    "hy2://",
    "VLESS://",
    "",
];

const SNIPPETS: &[&str] = &[
    "?",
    "#",
    "@",
    ":",
    "&",
    "=",
    "%",
    "%zz",
    "%00",
    "[",
    "]",
    "[::1]",
    ":99999999999",
    ":0",
    ":65536",
    "&type=",
    "&type=xhttp&extra=",
    "&extra=%7B",
    "&extra=%5B1%5D",
    "&security=reality",
    "&security=tls&pcs=",
    "&pqv=",
    "&plugin=obfs-local",
    "&aid=99999999999",
    "&mode=multi",
    "&headerType=http",
    "é",
    "日本",
    "🚀",
    "\u{202e}",
    "\u{0}",
    " ",
    "\n",
    "\"",
    "ss://",
    "vmess://",
];

const JSON_VALUES: &[&str] = &[
    "null",
    "0",
    "-1",
    "443",
    "\"443\"",
    "\"\"",
    "65536",
    "1e3",
    "443.0",
    "18446744073709551616",
    "true",
    "[]",
    "[\"h2\",1,null]",
    "{}",
    "\"tls\"",
    "\"reality\"",
    "\"grpc\"",
    "\"xhttp\"",
    "\"kcp\"",
    "\"quic\"",
    "\"日本\"",
];

const VMESS_KEYS: &[&str] = &[
    "v", "ps", "add", "port", "id", "aid", "scy", "net", "type", "host", "path", "tls", "sni",
    "alpn", "fp", "insecure",
];

fn mutate_bytes(rng: &mut Rng, input: &str) -> String {
    let mut b = input.as_bytes().to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        match rng.below(8) {
            // Truncate.
            0 => b.truncate(rng.below(b.len() + 1)),
            // Flip a byte.
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            // Insert a snippet (unicode, separators, huge ports…).
            2 | 3 => {
                let i = rng.below(b.len() + 1);
                let s = rng.pick(SNIPPETS).as_bytes();
                b.splice(i..i, s.iter().copied());
            }
            // Remove a range (empty fields).
            4 if !b.is_empty() => {
                let i = rng.below(b.len());
                let j = (i + rng.below(16)).min(b.len());
                b.drain(i..j);
            }
            // Duplicate a range.
            5 if !b.is_empty() => {
                let i = rng.below(b.len());
                let j = (i + rng.below(32)).min(b.len());
                let chunk: Vec<u8> = b[i..j].to_vec();
                let at = rng.below(b.len() + 1);
                b.splice(at..at, chunk);
            }
            // Random byte.
            _ => {
                let i = rng.below(b.len() + 1);
                b.insert(i, rng.next() as u8);
            }
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

/// A VMess JSON link with random field types.
fn random_vmess_json(rng: &mut Rng) -> String {
    let mut fields = Vec::new();
    for key in VMESS_KEYS {
        if rng.below(4) == 0 {
            continue;
        }
        let value = match *key {
            "add" if rng.below(2) == 0 => "\"h.example\"".to_owned(),
            "id" if rng.below(2) == 0 => format!("\"{}\"", corpus::UUID),
            "port" if rng.below(2) == 0 => "\"443\"".to_owned(),
            _ => (*rng.pick(JSON_VALUES)).to_owned(),
        };
        fields.push(format!("\"{key}\":{value}"));
    }
    let json = format!("{{{}}}", fields.join(","));
    let json = if rng.below(5) == 0 {
        mutate_bytes(rng, &json)
    } else {
        json
    };
    format!("vmess://{}", STANDARD.encode(json))
}

fn random_input(rng: &mut Rng, corpus: &[String]) -> String {
    match rng.below(10) {
        0 => {
            let len = rng.below(80);
            let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            format!("{}{}", rng.pick(SCHEMES), String::from_utf8_lossy(&bytes))
        }
        1 | 2 => random_vmess_json(rng),
        _ => {
            let base = rng.pick(corpus).clone();
            mutate_bytes(rng, &base)
        }
    }
}

fn check_server(server: &Server, rules: &[RouteRule], domains: &[String], n: usize) {
    let mut opts = ConfigOptions::new(server);
    opts.route_preset = [
        RoutePreset::All,
        RoutePreset::Smart,
        RoutePreset::OnlyListed,
    ][n % 3];
    opts.tunnel = n.is_multiple_of(2);
    opts.api_port = Some(10085);
    opts.custom_rules = rules;
    opts.listed_domains = domains;
    let cfg = build(&opts);
    let text = serde_json::to_string(&cfg).expect("config serialises");
    assert!(text.contains("\"proxy\""));
    let exported = try_to_link(server).expect("parsed servers are consistent");
    let again = parse_link(&exported)
        .unwrap_or_else(|e| panic!("export of {server:?} does not re-import ({exported}): {e}"));
    assert_eq!(&again, server, "exported: {exported}");
}

#[test]
fn random_and_mutated_input_never_panics() {
    let corpus = corpus::links();
    let rules = vec![
        RouteRule {
            pattern: "example.com".into(),
            target: RuleTarget::Direct,
        },
        RouteRule {
            pattern: "10.0.0.0/8".into(),
            target: RuleTarget::Block,
        },
    ];
    let domains = vec!["example.org".to_owned()];
    let iterations: usize = std::env::var("OBSCURE_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40_000);
    let seed: u64 = std::env::var("OBSCURE_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|s| *s != 0)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut rng = Rng(seed);
    let mut parsed = 0usize;
    let mut batch = String::new();
    for n in 0..iterations {
        let input = random_input(&mut rng, &corpus);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = looks_like_link(&input);
            if let Ok(server) = parse_link(&input) {
                check_server(&server, &rules, &domains, n);
                true
            } else {
                false
            }
        }));
        match result {
            Ok(true) => parsed += 1,
            Ok(false) => {}
            Err(_) => panic!("iteration {n} panicked on input {input:?}"),
        }
        batch.push_str(&input);
        batch.push('\n');
        if n % 64 == 63 {
            let text = std::mem::take(&mut batch);
            let result = catch_unwind(AssertUnwindSafe(|| parse_many(&text)));
            assert!(result.is_ok(), "parse_many panicked on {text:?}");
        }
    }
    eprintln!("fuzz: {iterations} inputs, {parsed} parsed into servers");
    // The mutations keep a fair share of the inputs valid; if this drops to
    // zero the generator broke and the run proves nothing.
    assert!(parsed > iterations / 20, "only {parsed} inputs parsed");
}
