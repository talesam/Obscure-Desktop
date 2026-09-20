//! Generates the Xray `config.json` for a server and connection preferences.
//! See docs/PLANO.md §3.4.

use serde_json::{Value, json};

use crate::links::{Auth, Protocol, Security, Server, Transport};
use crate::profile::RoutePreset;

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
        }
    }
}

/// Builds the full Xray configuration as JSON.
pub fn build(opts: &ConfigOptions) -> Value {
    let mut cfg = json!({
        "log": { "loglevel": opts.log_level },
        "dns": dns(opts),
        "inbounds": [ {
            "tag": "mixed-in",
            "listen": "127.0.0.1",
            "port": opts.local_port,
            "protocol": "mixed",
            "settings": { "udp": true },
            "sniffing": { "enabled": true, "destOverride": ["http", "tls", "quic"] }
        } ],
        "outbounds": [
            outbound(opts.server),
            { "tag": "direct", "protocol": "freedom", "settings": { "domainStrategy": "UseIP" } },
            { "tag": "block", "protocol": "blackhole", "settings": {} }
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

fn dns(opts: &ConfigOptions) -> Value {
    // The DoH server itself must be resolved and reached; `https+local` makes
    // Xray talk to it directly, without looping through the proxy.
    let local = opts.dns.replacen("https://", "https+local://", 1);
    json!({
        "servers": [ local, "localhost" ],
        "queryStrategy": "UseIP"
    })
}

fn routing(opts: &ConfigOptions) -> Value {
    let mut rules = vec![
        // Never proxy the proxy itself.
        json!({ "type": "field", "domain": [ format!("full:{}", opts.server.address) ], "outboundTag": "direct" }),
    ];
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
            if !opts.listed_domains.is_empty() {
                let domains: Vec<String> = opts
                    .listed_domains
                    .iter()
                    .map(|d| format!("domain:{d}"))
                    .collect();
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
                "method": method, "password": password, "uot": true } ] });
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
            let mut peer = json!({ "endpoint": format!("{}:{}", server.address, server.port), "publicKey": public_key });
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
        _ => unreachable!("protocol/auth mismatch"),
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
        Transport::Xhttp { path, host, mode } => {
            ss["network"] = json!("xhttp");
            let mut x = json!({ "path": path.clone().unwrap_or_else(|| "/".into()) });
            if let Some(h) = host {
                x["host"] = json!(h);
            }
            if let Some(m) = mode {
                x["mode"] = json!(m);
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
        } => {
            ss["security"] = json!("tls");
            let mut t = json!({ "serverName": sni.clone().unwrap_or_else(|| server.address.clone()),
                                "allowInsecure": allow_insecure });
            if !alpn.is_empty() {
                t["alpn"] = json!(alpn);
            }
            if let Some(fp) = fingerprint {
                t["fingerprint"] = json!(fp);
            }
            ss["tlsSettings"] = t;
        }
        Security::Reality {
            sni,
            fingerprint,
            public_key,
            short_id,
            spider_x,
        } => {
            ss["security"] = json!("reality");
            ss["realitySettings"] = json!({
                "serverName": sni.clone().unwrap_or_else(|| server.address.clone()),
                "fingerprint": fingerprint.clone().unwrap_or_else(|| "chrome".into()),
                "publicKey": public_key,
                "shortId": short_id.clone().unwrap_or_default(),
                "spiderX": spider_x.clone().unwrap_or_else(|| "/".into()),
            });
        }
    }
    ss
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
        assert_eq!(cfg["inbounds"][0]["protocol"], "mixed");
        assert_eq!(cfg["outbounds"][0]["tag"], "proxy");
        assert_eq!(cfg["outbounds"][1]["tag"], "direct");
        assert_eq!(cfg["outbounds"][2]["tag"], "block");
        assert_eq!(cfg["dns"]["servers"][0], "https+local://1.1.1.1/dns-query");
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
}
