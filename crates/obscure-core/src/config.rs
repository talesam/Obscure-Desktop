//! Generates the Xray `config.json` for a server and connection preferences.
//! See docs/PLANO.md §3.4.

use std::net::IpAddr;

use serde_json::{Value, json};

use crate::links::{Auth, Protocol, Security, Server, Transport, xhttp_extra_object};
use crate::profile::{RoutePreset, RouteRule, RuleTarget};

/// TUN interface name created in tunnel mode.
pub const TUN_NAME: &str = "obscure0";

/// Everything needed to build a runtime configuration.
#[derive(Debug, Clone)]
pub struct ConfigOptions<'a> {
    pub server: &'a Server,
    pub local_port: u16,
    pub route_preset: RoutePreset,
    pub listed_domains: &'a [String],
    /// DNS-over-HTTPS server used for remote resolution.
    pub dns: &'a str,
    /// Enable the gRPC API (stats) on this port.
    pub api_port: Option<u16>,
    pub log_level: &'a str,
    /// Add the `tun` inbound (all system traffic). Needs capabilities.
    pub tunnel: bool,
    /// User rules evaluated before the preset.
    pub custom_rules: &'a [RouteRule],
}

impl<'a> ConfigOptions<'a> {
    pub fn new(server: &'a Server) -> Self {
        Self {
            server,
            local_port: crate::DEFAULT_LOCAL_PORT,
            route_preset: RoutePreset::default(),
            listed_domains: &[],
            dns: "https://1.1.1.1/dns-query",
            api_port: None,
            log_level: "warning",
            tunnel: false,
            custom_rules: &[],
        }
    }
}

/// Builds the full Xray configuration as JSON.
pub fn build(opts: &ConfigOptions) -> Value {
    let mut cfg = json!({
        // `access: ""` sends one line per connection to stdout, which feeds
        // the connections view; DNS chatter stays off.
        "log": { "loglevel": opts.log_level, "access": "", "dnsLog": false },
        "dns": dns(opts),
        "inbounds": inbounds(opts),
        "outbounds": [
            outbound(opts.server),
            // `freedom.settings.domainStrategy` is deprecated since Xray 26.x.
            { "tag": "direct", "protocol": "freedom", "settings": {}, "streamSettings": { "sockopt": { "domainStrategy": "UseIP" } } },
            { "tag": "block", "protocol": "blackhole", "settings": {} },
            // Answers DNS queries captured from the tunnel with Xray's own DNS.
            { "tag": "dns-out", "protocol": "dns", "settings": {} }
        ],
        "routing": routing(opts),
        "stats": {},
        "policy": {
            "system": { "statsOutboundUplink": true, "statsOutboundDownlink": true }
        }
    });

    if let Some(port) = opts.api_port {
        cfg["api"] = json!({ "tag": "api", "listen": format!("127.0.0.1:{port}"), "services": ["StatsService"] });
    }
    cfg
}

fn inbounds(opts: &ConfigOptions) -> Value {
    let mut list = vec![json!({
        "tag": "mixed-in",
        "listen": "127.0.0.1",
        "port": opts.local_port,
        "protocol": "mixed",
        "settings": { "udp": true },
        "sniffing": { "enabled": true, "destOverride": ["http", "tls", "quic"] }
    })];
    if opts.tunnel {
        list.push(json!({
            "tag": "tun-in",
            "protocol": "tun",
            "settings": {
                "name": TUN_NAME,
                "mtu": 1500,
                // 198.18.0.0/15 is reserved for benchmarking (RFC 2544) and
                // never routed on the internet; 172.19.x collides with Docker.
                "gateway": ["198.18.0.1/30", "fdfe:dcba:9876::1/126"],
                "autoSystemRoutingTable": ["0.0.0.0/0", "::/0"],
                "autoOutboundsInterface": "auto"
            },
            "sniffing": { "enabled": true, "destOverride": ["http", "tls", "quic"], "routeOnly": false }
        }));
    }
    Value::Array(list)
}

/// Domain matchers Xray understands; anything else is a plain domain.
const DOMAIN_PREFIXES: [&str; 5] = ["geosite:", "domain:", "full:", "regexp:", "keyword:"];

/// A user pattern normalised for Xray's routing.
#[derive(Debug, PartialEq)]
enum Pattern {
    Ip(String),
    Domain(String),
}

/// Classifies a user pattern. Returns `None` for patterns Xray would reject
/// (empty, a bare prefix such as `domain:`, or a CIDR with an impossible
/// prefix length): a single bad entry makes Xray refuse the whole config.
fn classify(pattern: &str) -> Option<Pattern> {
    let p = pattern.trim();
    if p.is_empty() {
        return None;
    }
    if let Some(rest) = p.strip_prefix("geoip:") {
        return (!rest.trim().is_empty()).then(|| Pattern::Ip(p.to_owned()));
    }
    if p.parse::<std::net::IpAddr>().is_ok() {
        return Some(Pattern::Ip(p.to_owned()));
    }
    if let Some((ip, len)) = p.split_once('/')
        && let Ok(addr) = ip.parse::<std::net::IpAddr>()
    {
        let max = if addr.is_ipv4() { 32 } else { 128 };
        return len
            .parse::<u8>()
            .ok()
            .filter(|l| *l <= max)
            .map(|_| Pattern::Ip(p.to_owned()));
    }
    domain_pattern(p).map(Pattern::Domain)
}

/// Keeps a known matcher prefix (`geosite:`, `full:`…) or adds `domain:`.
fn domain_pattern(pattern: &str) -> Option<String> {
    let p = pattern.trim();
    if p.is_empty() {
        return None;
    }
    match DOMAIN_PREFIXES.iter().find(|prefix| p.starts_with(*prefix)) {
        Some(prefix) => (!p[prefix.len()..].trim().is_empty()).then(|| p.to_owned()),
        None => Some(format!("domain:{p}")),
    }
}

/// Converts a user rule into an Xray routing rule, or `None` if its
/// pattern is not usable.
fn custom_rule(rule: &RouteRule) -> Option<Value> {
    let tag = match rule.target {
        RuleTarget::Proxy => "proxy",
        RuleTarget::Direct => "direct",
        RuleTarget::Block => "block",
    };
    Some(match classify(&rule.pattern)? {
        Pattern::Ip(ip) => json!({ "type": "field", "ip": [ip], "outboundTag": tag }),
        Pattern::Domain(d) => json!({ "type": "field", "domain": [d], "outboundTag": tag }),
    })
}

fn dns(opts: &ConfigOptions) -> Value {
    // The DoH server itself must be resolved and reached; `https+local` makes
    // Xray talk to it directly, without looping through the proxy.
    let local = opts.dns.replacen("https://", "https+local://", 1);
    // IPv4 only: with `UseIP` an empty AAAA answer makes Xray's outbound
    // dial wait for its 16 s timeout before the first connection goes
    // through (measured: 21 s vs 1.9 s to first traffic in tunnel mode).
    // Proxy servers rarely have IPv6 egress anyway.
    json!({
        "servers": [ local, "localhost" ],
        "queryStrategy": "UseIPv4"
    })
}

fn routing(opts: &ConfigOptions) -> Value {
    // Never proxy the proxy itself. A `full:` domain rule never matches a
    // connection to an IP literal, so that case needs an `ip` rule.
    let own = &opts.server.address;
    let mut rules = vec![if own.parse::<IpAddr>().is_ok() {
        json!({ "type": "field", "ip": [ own ], "outboundTag": "direct" })
    } else {
        json!({ "type": "field", "domain": [ format!("full:{own}") ], "outboundTag": "direct" })
    }];
    if opts.tunnel {
        // DNS from the system goes to Xray's resolver (DoH), not to the LAN resolver.
        rules.push(json!({ "type": "field", "inboundTag": ["tun-in"], "port": 53, "network": "udp,tcp", "outboundTag": "dns-out" }));
    }
    rules.extend(opts.custom_rules.iter().filter_map(custom_rule));
    match opts.route_preset {
        RoutePreset::All => {
            rules
                .push(json!({ "type": "field", "ip": ["geoip:private"], "outboundTag": "direct" }));
        }
        RoutePreset::Smart => {
            rules
                .push(json!({ "type": "field", "ip": ["geoip:private"], "outboundTag": "direct" }));
            rules.push(
                json!({ "type": "field", "domain": ["geosite:private"], "outboundTag": "direct" }),
            );
            rules.push(json!({ "type": "field", "domain": ["geosite:category-ads-all"], "outboundTag": "block" }));
        }
        RoutePreset::OnlyListed => {
            let domains: Vec<String> = opts
                .listed_domains
                .iter()
                .filter_map(|d| domain_pattern(d))
                .collect();
            if !domains.is_empty() {
                rules.push(json!({ "type": "field", "domain": domains, "outboundTag": "proxy" }));
            }
            rules.push(json!({ "type": "field", "network": "tcp,udp", "outboundTag": "direct" }));
        }
    }
    json!({
        "domainStrategy": "IPIfNonMatch",
        "rules": rules,
        // Anything unmatched goes to the first outbound (proxy).
    })
}

/// Builds the `proxy` outbound for a server.
pub fn outbound(server: &Server) -> Value {
    let mut out = json!({ "tag": "proxy" });
    match (&server.protocol, &server.auth) {
        (Protocol::Vless, Auth::Vless { uuid, flow }) => {
            let mut user = json!({ "id": uuid, "encryption": "none" });
            if let Some(flow) = flow {
                user["flow"] = json!(flow);
            }
            out["protocol"] = json!("vless");
            out["settings"] = json!({ "vnext": [ { "address": server.address, "port": server.port, "users": [ user ] } ] });
        }
        (
            Protocol::Vmess,
            Auth::Vmess {
                uuid,
                alter_id,
                security,
            },
        ) => {
            out["protocol"] = json!("vmess");
            out["settings"] = json!({ "vnext": [ { "address": server.address, "port": server.port,
                "users": [ { "id": uuid, "alterId": alter_id, "security": security } ] } ] });
        }
        (Protocol::Trojan, Auth::Trojan { password }) => {
            out["protocol"] = json!("trojan");
            out["settings"] = json!({ "servers": [ { "address": server.address, "port": server.port, "password": password } ] });
        }
        (Protocol::Shadowsocks, Auth::Shadowsocks { method, password }) => {
            out["protocol"] = json!("shadowsocks");
            out["settings"] = json!({ "servers": [ { "address": server.address, "port": server.port,
                "method": method, "password": password } ] });
        }
        (
            Protocol::Wireguard,
            Auth::Wireguard {
                private_key,
                public_key,
                preshared_key,
                address,
                reserved,
                mtu,
            },
        ) => {
            out["protocol"] = json!("wireguard");
            let mut peer = json!({ "endpoint": format!("{}:{}", server.url_host(), server.port), "publicKey": public_key });
            if let Some(psk) = preshared_key {
                peer["preSharedKey"] = json!(psk);
            }
            let mut settings = json!({
                "secretKey": private_key,
                "address": if address.is_empty() { vec!["172.16.0.2/32".to_owned()] } else { address.clone() },
                "peers": [ peer ],
                "domainStrategy": "ForceIP",
            });
            if !reserved.is_empty() {
                settings["reserved"] = json!(reserved);
            }
            if let Some(mtu) = mtu {
                settings["mtu"] = json!(mtu);
            }
            out["settings"] = settings;
            // WireGuard has its own transport; no streamSettings.
            return out;
        }
        (Protocol::Socks | Protocol::Http, Auth::UserPass { user, pass }) => {
            out["protocol"] = json!(if server.protocol == Protocol::Socks {
                "socks"
            } else {
                "http"
            });
            let mut srv = json!({ "address": server.address, "port": server.port });
            if let Some(u) = user {
                srv["users"] = json!([ { "user": u, "pass": pass.clone().unwrap_or_default() } ]);
            }
            out["settings"] = json!({ "servers": [ srv ] });
        }
        _ => {
            // Only reachable through a hand-edited profile (`Profiles::load`
            // drops such entries). A blackhole keeps Xray from leaking the
            // traffic directly instead of panicking.
            tracing::error!(
                "server `{}`: credentials do not match {}",
                server.name,
                server.protocol.label()
            );
            return json!({ "tag": "proxy", "protocol": "blackhole", "settings": {} });
        }
    }
    out["streamSettings"] = stream_settings(server);
    out
}

fn stream_settings(server: &Server) -> Value {
    let mut ss = json!({});
    match &server.transport {
        Transport::Tcp { header_type } => {
            ss["network"] = json!("tcp");
            if let Some(h) = header_type {
                ss["tcpSettings"] = json!({ "header": { "type": h } });
            }
        }
        Transport::Ws { path, host } => {
            ss["network"] = json!("ws");
            let mut w = json!({ "path": path.clone().unwrap_or_else(|| "/".into()) });
            if let Some(h) = host {
                w["host"] = json!(h);
            }
            ss["wsSettings"] = w;
        }
        Transport::Grpc { service_name, mode } => {
            ss["network"] = json!("grpc");
            let mut g = json!({ "serviceName": service_name.clone().unwrap_or_default() });
            if mode.as_deref() == Some("multi") {
                g["multiMode"] = json!(true);
            }
            ss["grpcSettings"] = g;
        }
        Transport::Xhttp {
            path,
            host,
            mode,
            extra,
        } => {
            ss["network"] = json!("xhttp");
            let mut x = json!({ "path": path.clone().unwrap_or_else(|| "/".into()) });
            if let Some(h) = host {
                x["host"] = json!(h);
            }
            if let Some(m) = mode {
                x["mode"] = json!(m);
            }
            // Xray merges `extra` into these settings itself.
            match extra.as_deref().map(xhttp_extra_object) {
                Some(Ok(map)) => x["extra"] = Value::Object(map),
                Some(Err(e)) => tracing::warn!("server `{}`: {e}; ignoring it", server.name),
                None => {}
            }
            ss["xhttpSettings"] = x;
        }
        Transport::HttpUpgrade { path, host } => {
            ss["network"] = json!("httpupgrade");
            let mut h = json!({ "path": path.clone().unwrap_or_else(|| "/".into()) });
            if let Some(host) = host {
                h["host"] = json!(host);
            }
            ss["httpupgradeSettings"] = h;
        }
        Transport::Kcp { seed, header_type } => {
            ss["network"] = json!("kcp");
            let mut k = json!({ "header": { "type": header_type.clone().unwrap_or_else(|| "none".into()) } });
            if let Some(seed) = seed {
                k["seed"] = json!(seed);
            }
            ss["kcpSettings"] = k;
        }
    }
    match &server.security {
        Security::None => {
            ss["security"] = json!("none");
        }
        Security::Tls {
            sni,
            alpn,
            fingerprint,
            allow_insecure,
            pinned_peer_cert_sha256,
            verify_peer_cert_by_name,
        } => {
            ss["security"] = json!("tls");
            let mut t = json!({ "serverName": tls_server_name(server, sni.as_deref()) });
            if !alpn.is_empty() {
                t["alpn"] = json!(alpn);
            }
            if let Some(fp) = fingerprint {
                t["fingerprint"] = json!(fp);
            }
            if let Some(pcs) = pinned_peer_cert_sha256 {
                t["pinnedPeerCertSha256"] = json!(pcs);
            }
            if let Some(vcn) = verify_peer_cert_by_name {
                t["verifyPeerCertByName"] = json!(vcn);
            }
            // Xray 26 removed `allowInsecure` and rejects any config that
            // still enables it; `pcs`/`vcn` replace it.
            if *allow_insecure {
                tracing::warn!(
                    "server `{}` asks to skip certificate checks, which Xray no longer supports; verifying normally",
                    server.name
                );
            }
            ss["tlsSettings"] = t;
        }
        Security::Reality {
            sni,
            fingerprint,
            public_key,
            short_id,
            spider_x,
            mldsa65_verify,
        } => {
            ss["security"] = json!("reality");
            let mut r = json!({
                "serverName": sni.clone().unwrap_or_else(|| server.address.clone()),
                "fingerprint": fingerprint.clone().unwrap_or_else(|| "chrome".into()),
                "publicKey": public_key,
                "shortId": short_id.clone().unwrap_or_default(),
                "spiderX": spider_x.clone().unwrap_or_else(|| "/".into()),
            });
            if let Some(pqv) = mldsa65_verify {
                r["mldsa65Verify"] = json!(pqv);
            }
            ss["realitySettings"] = r;
        }
    }
    ss
}

/// TLS `serverName`: the link's SNI, else the address. An IP literal is
/// never a valid SNI, so behind a CDN (address = IP) the transport's
/// `host` header, which names the real site, is used when present.
fn tls_server_name(server: &Server, sni: Option<&str>) -> String {
    if let Some(sni) = sni.map(str::trim).filter(|s| !s.is_empty()) {
        return sni.to_owned();
    }
    if server.address.parse::<IpAddr>().is_ok() {
        let host = match &server.transport {
            Transport::Ws { host, .. }
            | Transport::HttpUpgrade { host, .. }
            | Transport::Xhttp { host, .. } => host.as_deref(),
            _ => None,
        };
        if let Some(h) = host
            .and_then(|h| h.split(',').map(str::trim).find(|h| !h.is_empty()))
            .filter(|h| h.parse::<IpAddr>().is_err())
        {
            return h.to_owned();
        }
    }
    server.address.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::links::parse_link;

    const UUID: &str = "2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c";

    #[test]
    fn vless_reality_outbound() {
        let s = parse_link(&format!(
            "vless://{UUID}@example.org:443?flow=xtls-rprx-vision&fp=random&pbk=PBK&security=reality&sid=95&sni=www.yahoo.com&spx=%2Fabc&type=tcp#R"
        ))
        .unwrap();
        let o = outbound(&s);
        assert_eq!(o["protocol"], "vless");
        assert_eq!(
            o["settings"]["vnext"][0]["users"][0]["flow"],
            "xtls-rprx-vision"
        );
        assert_eq!(o["settings"]["vnext"][0]["users"][0]["encryption"], "none");
        assert_eq!(o["streamSettings"]["network"], "tcp");
        assert_eq!(o["streamSettings"]["security"], "reality");
        let r = &o["streamSettings"]["realitySettings"];
        assert_eq!(r["serverName"], "www.yahoo.com");
        assert_eq!(r["fingerprint"], "random");
        assert_eq!(r["publicKey"], "PBK");
        assert_eq!(r["shortId"], "95");
        assert_eq!(r["spiderX"], "/abc");
    }

    #[test]
    fn full_config_smart_preset() {
        let s = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=tls&type=ws&path=%2Fws#W"
        ))
        .unwrap();
        let mut opts = ConfigOptions::new(&s);
        opts.api_port = Some(10085);
        let cfg = build(&opts);
        assert_eq!(cfg["inbounds"][0]["port"], 2080);
        assert_eq!(cfg["log"]["access"], "");
        assert_eq!(cfg["inbounds"][0]["protocol"], "mixed");
        assert_eq!(cfg["outbounds"][0]["tag"], "proxy");
        assert_eq!(cfg["outbounds"][1]["tag"], "direct");
        assert_eq!(cfg["outbounds"][2]["tag"], "block");
        assert_eq!(cfg["dns"]["servers"][0], "https+local://1.1.1.1/dns-query");
        assert_eq!(cfg["dns"]["queryStrategy"], "UseIPv4");
        assert_eq!(cfg["api"]["listen"], "127.0.0.1:10085");
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["domain"][0], "full:h.example");
        assert!(rules.iter().any(|r| r["outboundTag"] == "block"));
        assert_eq!(
            cfg["outbounds"][0]["streamSettings"]["wsSettings"]["path"],
            "/ws"
        );
        assert_eq!(
            cfg["outbounds"][0]["streamSettings"]["tlsSettings"]["serverName"],
            "h.example"
        );
    }

    #[test]
    fn tunnel_mode_adds_tun_inbound_and_dns_capture() {
        let s = parse_link(&format!("vless://{UUID}@h.example:443?security=none#T")).unwrap();
        let mut opts = ConfigOptions::new(&s);
        opts.tunnel = true;
        let cfg = build(&opts);
        let inbounds = cfg["inbounds"].as_array().unwrap();
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[1]["protocol"], "tun");
        assert_eq!(inbounds[1]["settings"]["name"], TUN_NAME);
        assert_eq!(inbounds[1]["settings"]["autoOutboundsInterface"], "auto");
        assert!(
            cfg["outbounds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["tag"] == "dns-out")
        );
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert!(
            rules
                .iter()
                .any(|r| r["outboundTag"] == "dns-out" && r["port"] == 53)
        );
        // Without tunnel: no tun inbound and no dns capture rule.
        let plain = build(&ConfigOptions::new(&s));
        assert_eq!(plain["inbounds"].as_array().unwrap().len(), 1);
        assert!(
            !plain["routing"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["outboundTag"] == "dns-out")
        );
    }

    #[test]
    fn custom_rules_come_before_preset() {
        let s = parse_link(&format!("vless://{UUID}@h.example:443?security=none#T")).unwrap();
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
                pattern: "geosite:netflix".into(),
                target: RuleTarget::Proxy,
            },
            RouteRule {
                pattern: "1.2.3.4".into(),
                target: RuleTarget::Proxy,
            },
        ];
        let mut opts = ConfigOptions::new(&s);
        opts.custom_rules = &rules;
        let cfg = build(&opts);
        let r = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(r[1]["domain"][0], "domain:example.com");
        assert_eq!(r[1]["outboundTag"], "direct");
        assert_eq!(r[2]["ip"][0], "10.0.0.0/8");
        assert_eq!(r[3]["domain"][0], "geosite:netflix");
        assert_eq!(r[4]["ip"][0], "1.2.3.4");
        // Preset rules follow.
        assert!(r[5..].iter().any(|x| x["outboundTag"] == "block"));
    }

    #[test]
    fn only_listed_preset_defaults_to_direct() {
        let s = parse_link("trojan://pw@h.example:443#T").unwrap();
        let domains = vec!["example.com".to_owned()];
        let mut opts = ConfigOptions::new(&s);
        opts.route_preset = RoutePreset::OnlyListed;
        opts.listed_domains = &domains;
        let cfg = build(&opts);
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(rules[1]["domain"][0], "domain:example.com");
        assert_eq!(rules[1]["outboundTag"], "proxy");
        assert_eq!(rules.last().unwrap()["outboundTag"], "direct");
    }

    #[test]
    fn listed_domains_keep_prefixes_and_skip_blanks() {
        let s = parse_link("trojan://pw@h.example:443#T").unwrap();
        let domains: Vec<String> = [
            " example.com ",
            "",
            "   ",
            "domain:",
            "geosite:netflix",
            "full:a.example",
            "regexp:^x\\.",
            "keyword:tube",
            "domain:b.example",
        ]
        .iter()
        .map(|d| (*d).to_owned())
        .collect();
        let mut opts = ConfigOptions::new(&s);
        opts.route_preset = RoutePreset::OnlyListed;
        opts.listed_domains = &domains;
        let cfg = build(&opts);
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert_eq!(
            rules[1]["domain"],
            json!([
                "domain:example.com",
                "geosite:netflix",
                "full:a.example",
                "regexp:^x\\.",
                "keyword:tube",
                "domain:b.example"
            ])
        );
        // Only blanks: no proxy rule at all (an empty list is rejected by Xray).
        let blanks = vec![" ".to_owned(), "domain:".to_owned()];
        opts.listed_domains = &blanks;
        let cfg = build(&opts);
        let rules = cfg["routing"]["rules"].as_array().unwrap();
        assert!(!rules.iter().any(|r| r["outboundTag"] == "proxy"));
    }

    #[test]
    fn invalid_custom_rules_are_skipped() {
        let s = parse_link(&format!("vless://{UUID}@h.example:443?security=none#T")).unwrap();
        let rule = |p: &str| RouteRule {
            pattern: p.into(),
            target: RuleTarget::Direct,
        };
        let rules = vec![
            rule(""),
            rule("  "),
            rule("domain:"),
            rule("geoip:"),
            rule("10.0.0.0/33"),
            rule("fd00::/129"),
            rule("10.0.0.0/x"),
            rule("10.0.0.0/32"),
            rule("fd00::/128"),
        ];
        let mut opts = ConfigOptions::new(&s);
        opts.custom_rules = &rules;
        let cfg = build(&opts);
        let r = cfg["routing"]["rules"].as_array().unwrap();
        let direct: Vec<&Value> = r[1..]
            .iter()
            .filter(|x| x["outboundTag"] == "direct")
            .collect();
        assert_eq!(direct[0]["ip"][0], "10.0.0.0/32");
        assert_eq!(direct[1]["ip"][0], "fd00::/128");
        assert!(
            r.iter()
                .all(|x| x["domain"] != json!(["domain:"]) && x["ip"] != json!(["geoip:"]))
        );
        assert!(!r.iter().any(|x| {
            x["domain"][0]
                .as_str()
                .is_some_and(|d| d.contains('/') || d.trim() == "domain:")
        }));
    }

    #[test]
    fn wireguard_ipv6_endpoint_is_bracketed() {
        let wg = parse_link("wireguard://priv@[2001:db8::1]:51820?publickey=pub#W").unwrap();
        let o = outbound(&wg);
        assert_eq!(o["settings"]["peers"][0]["endpoint"], "[2001:db8::1]:51820");
    }

    #[test]
    fn wireguard_and_socks_outbounds() {
        let wg = parse_link(
            "wireguard://priv@wg.example:51820?publickey=pub&address=10.0.0.2/32&reserved=1,2,3#W",
        )
        .unwrap();
        let o = outbound(&wg);
        assert_eq!(o["protocol"], "wireguard");
        assert_eq!(o["settings"]["peers"][0]["endpoint"], "wg.example:51820");
        assert_eq!(o["settings"]["reserved"], json!([1, 2, 3]));
        assert!(o.get("streamSettings").is_none());

        let socks = parse_link("socks://u:p@s.example:1080#S").unwrap();
        let o = outbound(&socks);
        assert_eq!(o["protocol"], "socks");
        assert_eq!(o["settings"]["servers"][0]["users"][0]["user"], "u");
    }

    #[test]
    fn shadowsocks_and_vmess_outbounds() {
        let ss = parse_link("ss://2022-blake3-aes-128-gcm:key@h.example:8388#S").unwrap();
        let o = outbound(&ss);
        assert_eq!(o["protocol"], "shadowsocks");
        assert_eq!(
            o["settings"]["servers"][0]["method"],
            "2022-blake3-aes-128-gcm"
        );

        let vm = parse_link(&format!(
            "vmess://{UUID}@h.example:443?type=grpc&serviceName=g&mode=multi&security=tls"
        ))
        .unwrap();
        let o = outbound(&vm);
        assert_eq!(o["protocol"], "vmess");
        assert_eq!(o["streamSettings"]["grpcSettings"]["multiMode"], true);
    }

    #[test]
    fn mismatched_server_builds_a_blackhole_instead_of_panicking() {
        let mut s = parse_link(&format!("vless://{UUID}@h.example:443#V")).unwrap();
        s.auth = Auth::Trojan {
            password: "pw".into(),
        };
        let cfg = build(&ConfigOptions::new(&s));
        assert_eq!(cfg["outbounds"][0]["tag"], "proxy");
        assert_eq!(cfg["outbounds"][0]["protocol"], "blackhole");
        assert!(serde_json::to_string(&cfg).is_ok());
    }

    fn tls_name(link: &str) -> Value {
        let s = parse_link(link).unwrap();
        outbound(&s)["streamSettings"]["tlsSettings"]["serverName"].clone()
    }

    #[test]
    fn tls_server_name_falls_back_to_transport_host_for_ip_addresses() {
        // Address is an IP: the CDN host names the site.
        for t in ["ws", "httpupgrade", "xhttp"] {
            assert_eq!(
                tls_name(&format!(
                    "vless://{UUID}@104.16.1.1:443?type={t}&host=cdn.example&security=tls"
                )),
                "cdn.example",
                "{t}"
            );
        }
        assert_eq!(
            tls_name(&format!(
                "vless://{UUID}@[2606:4700::1]:443?type=ws&host=v6.example&security=tls"
            )),
            "v6.example"
        );
        // Explicit SNI always wins.
        assert_eq!(
            tls_name(&format!(
                "vless://{UUID}@104.16.1.1:443?type=ws&host=cdn.example&sni=sni.example&security=tls"
            )),
            "sni.example"
        );
        // A domain address keeps its own name, whatever the host header says.
        assert_eq!(
            tls_name(&format!(
                "vless://{UUID}@h.example:443?type=ws&host=cdn.example&security=tls"
            )),
            "h.example"
        );
        // IP without a host: nothing better than the address.
        assert_eq!(
            tls_name(&format!(
                "vless://{UUID}@104.16.1.1:443?type=ws&security=tls"
            )),
            "104.16.1.1"
        );
        assert_eq!(
            tls_name(&format!(
                "vless://{UUID}@104.16.1.1:443?type=ws&host=10.0.0.1&security=tls"
            )),
            "104.16.1.1"
        );
    }

    #[test]
    fn own_ip_address_gets_an_ip_rule() {
        for (addr, host) in [
            ("203.0.113.9", "203.0.113.9"),
            ("2001:db8::7", "[2001:db8::7]"),
        ] {
            let s = parse_link(&format!("vless://{UUID}@{host}:443?security=none#T")).unwrap();
            let cfg = build(&ConfigOptions::new(&s));
            let first = &cfg["routing"]["rules"][0];
            assert_eq!(first["ip"], json!([addr]));
            assert!(first.get("domain").is_none());
            assert_eq!(first["outboundTag"], "direct");
        }
        let s = parse_link(&format!("vless://{UUID}@h.example:443?security=none#T")).unwrap();
        let cfg = build(&ConfigOptions::new(&s));
        assert_eq!(
            cfg["routing"]["rules"][0]["domain"],
            json!(["full:h.example"])
        );
    }

    #[test]
    fn no_uot_and_no_allow_insecure_keys() {
        let ss = parse_link("ss://2022-blake3-aes-128-gcm:key@h.example:8388#S").unwrap();
        assert!(outbound(&ss)["settings"]["servers"][0].get("uot").is_none());
        for flag in ["", "&allowInsecure=1"] {
            let s =
                parse_link(&format!("vless://{UUID}@h.example:443?security=tls{flag}")).unwrap();
            let t = &outbound(&s)["streamSettings"]["tlsSettings"];
            assert!(t.get("allowInsecure").is_none(), "{flag}: {t}");
        }
    }

    #[test]
    fn pinning_pq_key_and_xhttp_extra_reach_the_config() {
        let s = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=tls&pcs=ABCD&vcn=real.example&type=xhttp&mode=auto&extra=%7B%22xPaddingBytes%22%3A%22100-1000%22%7D"
        ))
        .unwrap();
        let ss = &outbound(&s)["streamSettings"];
        assert_eq!(ss["tlsSettings"]["pinnedPeerCertSha256"], "ABCD");
        assert_eq!(ss["tlsSettings"]["verifyPeerCertByName"], "real.example");
        assert_eq!(ss["xhttpSettings"]["mode"], "auto");
        assert_eq!(
            ss["xhttpSettings"]["extra"],
            json!({ "xPaddingBytes": "100-1000" })
        );

        let r = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=reality&pbk=PBK&pqv=PQ"
        ))
        .unwrap();
        assert_eq!(
            outbound(&r)["streamSettings"]["realitySettings"]["mldsa65Verify"],
            "PQ"
        );
        let plain = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=reality&pbk=PBK"
        ))
        .unwrap();
        assert!(
            outbound(&plain)["streamSettings"]["realitySettings"]
                .get("mldsa65Verify")
                .is_none()
        );
    }
}
