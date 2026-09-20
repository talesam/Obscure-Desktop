//! Parsing and serialising of share links: `vless://`, `vmess://`,
//! `trojan://` and `ss://`.
//!
//! References: XTLS share-link standard (Xray-core discussion #716), v2rayN
//! VMess base64 JSON, SIP002 for Shadowsocks.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

/// Characters percent-encoded in fragments and query values we emit.
const ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Vless => "VLESS",
            Protocol::Vmess => "VMess",
            Protocol::Trojan => "Trojan",
            Protocol::Shadowsocks => "Shadowsocks",
        }
    }
}

/// Protocol-specific credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Auth {
    Vless {
        uuid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
    },
    Vmess {
        uuid: String,
        #[serde(default)]
        alter_id: u32,
        /// `auto`, `aes-128-gcm`, `chacha20-poly1305`, `none`, `zero`.
        #[serde(default = "default_vmess_security")]
        security: String,
    },
    Trojan {
        password: String,
    },
    Shadowsocks {
        method: String,
        password: String,
    },
}

fn default_vmess_security() -> String {
    "auto".into()
}

/// Stream transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Transport {
    Tcp {
        /// `http` enables the HTTP obfuscation header.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header_type: Option<String>,
    },
    Ws {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    Grpc {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
    Xhttp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
    HttpUpgrade {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        host: Option<String>,
    },
    Kcp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header_type: Option<String>,
    },
}

impl Transport {
    pub fn label(&self) -> &'static str {
        match self {
            Transport::Tcp { .. } => "TCP",
            Transport::Ws { .. } => "WebSocket",
            Transport::Grpc { .. } => "gRPC",
            Transport::Xhttp { .. } => "XHTTP",
            Transport::HttpUpgrade { .. } => "HTTPUpgrade",
            Transport::Kcp { .. } => "mKCP",
        }
    }

    /// Value used in the `type=` query parameter of share links.
    fn link_type(&self) -> &'static str {
        match self {
            Transport::Tcp { .. } => "tcp",
            Transport::Ws { .. } => "ws",
            Transport::Grpc { .. } => "grpc",
            Transport::Xhttp { .. } => "xhttp",
            Transport::HttpUpgrade { .. } => "httpupgrade",
            Transport::Kcp { .. } => "kcp",
        }
    }
}

/// Transport-layer security.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Security {
    None,
    Tls {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sni: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        alpn: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        #[serde(default)]
        allow_insecure: bool,
    },
    Reality {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sni: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
        public_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        short_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spider_x: Option<String>,
    },
}

impl Security {
    pub fn label(&self) -> &'static str {
        match self {
            Security::None => "sem TLS",
            Security::Tls { .. } => "TLS",
            Security::Reality { .. } => "REALITY",
        }
    }
}

/// A single server as described by a share link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    pub name: String,
    pub protocol: Protocol,
    pub address: String,
    pub port: u16,
    pub auth: Auth,
    pub transport: Transport,
    pub security: Security,
}

impl Server {
    /// Human-friendly fallback name when the link carries none.
    pub fn default_name(address: &str, port: u16) -> String {
        format!("{address}:{port}")
    }
}

/// Returns `true` if the text looks like a share link we can import.
pub fn looks_like_link(text: &str) -> bool {
    let t = text.trim();
    ["vless://", "vmess://", "trojan://", "ss://"]
        .iter()
        .any(|p| t.len() > p.len() && t[..p.len()].eq_ignore_ascii_case(p))
}

/// Parses every share link found in `text` (one per line). Lines that are
/// not links are ignored; lines that are links but fail to parse are
/// returned as errors alongside the successes.
pub fn parse_many(text: &str) -> (Vec<Server>, Vec<(String, Error)>) {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !looks_like_link(line) {
            continue;
        }
        match parse_link(line) {
            Ok(server) => ok.push(server),
            Err(e) => failed.push((line.to_owned(), e)),
        }
    }
    (ok, failed)
}

/// Parses one share link.
pub fn parse_link(link: &str) -> Result<Server> {
    let link = link.trim();
    let (scheme, _) = link
        .split_once("://")
        .ok_or_else(|| Error::MalformedLink("missing `://`".into()))?;
    match scheme.to_ascii_lowercase().as_str() {
        "vless" => parse_vless(link),
        "vmess" => parse_vmess(link),
        "trojan" => parse_trojan(link),
        "ss" => parse_shadowsocks(link),
        other => Err(Error::UnsupportedScheme(other.to_owned())),
    }
}

// ---------------------------------------------------------------------------
// helpers

struct Parsed {
    userinfo: String,
    host: String,
    port: u16,
    query: HashMap<String, String>,
    name: Option<String>,
}

fn parse_url_like(link: &str) -> Result<Parsed> {
    let url = Url::parse(link).map_err(|e| Error::MalformedLink(e.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::MalformedLink("missing host".into()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url
        .port()
        .ok_or_else(|| Error::MalformedLink("missing port".into()))?;
    // `user:pass@host` is split by the url crate; join it back (Shadowsocks).
    let userinfo = match url.password() {
        Some(pw) => format!("{}:{}", decode(url.username()), decode(pw)),
        None => decode(url.username()),
    };
    let query = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let name = url
        .fragment()
        .map(decode)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    Ok(Parsed {
        userinfo,
        host,
        port,
        query,
        name,
    })
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

fn encode(s: &str) -> String {
    utf8_percent_encode(s, ENCODE_SET).to_string()
}

fn opt(q: &HashMap<String, String>, key: &str) -> Option<String> {
    q.get(key)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn transport_from_query(q: &HashMap<String, String>) -> Transport {
    let kind = opt(q, "type").unwrap_or_else(|| "tcp".into());
    let header_type = opt(q, "headerType").filter(|h| h != "none");
    match kind.to_ascii_lowercase().as_str() {
        "ws" => Transport::Ws {
            path: opt(q, "path"),
            host: opt(q, "host"),
        },
        "grpc" | "gun" => Transport::Grpc {
            service_name: opt(q, "serviceName"),
            mode: opt(q, "mode"),
        },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: opt(q, "path"),
            host: opt(q, "host"),
            mode: opt(q, "mode"),
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: opt(q, "path"),
            host: opt(q, "host"),
        },
        "kcp" | "mkcp" => Transport::Kcp {
            seed: opt(q, "seed"),
            header_type,
        },
        _ => Transport::Tcp { header_type },
    }
}

fn security_from_query(q: &HashMap<String, String>, default_tls: bool) -> Result<Security> {
    let kind = opt(q, "security").unwrap_or_else(|| {
        if default_tls {
            "tls".into()
        } else {
            "none".into()
        }
    });
    Ok(match kind.to_ascii_lowercase().as_str() {
        "none" | "" => Security::None,
        "tls" | "xtls" => Security::Tls {
            sni: opt(q, "sni"),
            alpn: opt(q, "alpn")
                .map(|a| a.split(',').map(|s| s.trim().to_owned()).collect())
                .unwrap_or_default(),
            fingerprint: opt(q, "fp"),
            allow_insecure: matches!(
                opt(q, "allowInsecure")
                    .or_else(|| opt(q, "insecure"))
                    .as_deref(),
                Some("1") | Some("true")
            ),
        },
        "reality" => Security::Reality {
            sni: opt(q, "sni"),
            fingerprint: opt(q, "fp"),
            public_key: opt(q, "pbk")
                .ok_or_else(|| Error::MalformedLink("reality link without `pbk`".into()))?,
            short_id: opt(q, "sid"),
            spider_x: opt(q, "spx"),
        },
        other => return Err(Error::MalformedLink(format!("unknown security `{other}`"))),
    })
}

fn add(params: &mut Vec<(String, String)>, k: &str, v: &Option<String>) {
    if let Some(v) = v {
        params.push((k.into(), v.clone()));
    }
}

fn push_transport_params(t: &Transport, params: &mut Vec<(String, String)>) {
    params.push(("type".into(), t.link_type().into()));
    match t {
        Transport::Tcp { header_type } => add(params, "headerType", header_type),
        Transport::Ws { path, host } | Transport::HttpUpgrade { path, host } => {
            add(params, "path", path);
            add(params, "host", host);
        }
        Transport::Grpc { service_name, mode } => {
            add(params, "serviceName", service_name);
            add(params, "mode", mode);
        }
        Transport::Xhttp { path, host, mode } => {
            add(params, "path", path);
            add(params, "host", host);
            add(params, "mode", mode);
        }
        Transport::Kcp { seed, header_type } => {
            add(params, "seed", seed);
            add(params, "headerType", header_type);
        }
    }
}

fn push_security_params(s: &Security, params: &mut Vec<(String, String)>) {
    match s {
        Security::None => params.push(("security".into(), "none".into())),
        Security::Tls {
            sni,
            alpn,
            fingerprint,
            allow_insecure,
        } => {
            params.push(("security".into(), "tls".into()));
            add(params, "sni", sni);
            if !alpn.is_empty() {
                params.push(("alpn".into(), alpn.join(",")));
            }
            add(params, "fp", fingerprint);
            if *allow_insecure {
                params.push(("allowInsecure".into(), "1".into()));
            }
        }
        Security::Reality {
            sni,
            fingerprint,
            public_key,
            short_id,
            spider_x,
        } => {
            params.push(("security".into(), "reality".into()));
            add(params, "sni", sni);
            add(params, "fp", fingerprint);
            params.push(("pbk".into(), public_key.clone()));
            add(params, "sid", short_id);
            add(params, "spx", spider_x);
        }
    }
}

fn build_url_like(
    scheme: &str,
    userinfo: &str,
    server: &Server,
    params: Vec<(String, String)>,
) -> String {
    let host = if server.address.contains(':') {
        format!("[{}]", server.address)
    } else {
        server.address.clone()
    };
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    let mut out = format!("{scheme}://{}@{host}:{}", encode(userinfo), server.port);
    if !query.is_empty() {
        out.push('?');
        out.push_str(&query.join("&"));
    }
    out.push('#');
    out.push_str(&encode(&server.name));
    out
}

// ---------------------------------------------------------------------------
// VLESS

fn parse_vless(link: &str) -> Result<Server> {
    let p = parse_url_like(link)?;
    if p.userinfo.is_empty() {
        return Err(Error::MalformedLink("vless link without uuid".into()));
    }
    let encryption = opt(&p.query, "encryption").unwrap_or_else(|| "none".into());
    if encryption != "none" {
        return Err(Error::MalformedLink(format!(
            "unsupported vless encryption `{encryption}`"
        )));
    }
    let name = p
        .name
        .clone()
        .unwrap_or_else(|| Server::default_name(&p.host, p.port));
    Ok(Server {
        name,
        protocol: Protocol::Vless,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Vless {
            uuid: p.userinfo.clone(),
            flow: opt(&p.query, "flow"),
        },
        transport: transport_from_query(&p.query),
        security: security_from_query(&p.query, false)?,
    })
}

// ---------------------------------------------------------------------------
// VMess

#[derive(Debug, Default, Serialize, Deserialize)]
struct VmessJson {
    #[serde(default)]
    v: serde_json::Value,
    #[serde(default)]
    ps: String,
    #[serde(default)]
    add: String,
    #[serde(default)]
    port: serde_json::Value,
    #[serde(default)]
    id: String,
    #[serde(default)]
    aid: serde_json::Value,
    #[serde(default)]
    scy: String,
    #[serde(default)]
    net: String,
    #[serde(default, rename = "type")]
    header_type: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    tls: String,
    #[serde(default)]
    sni: String,
    #[serde(default)]
    alpn: String,
    #[serde(default)]
    fp: String,
}

fn value_to_u32(v: &serde_json::Value) -> Option<u32> {
    match v {
        serde_json::Value::Number(n) => n.as_u64().map(|n| n as u32),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn decode_base64_lenient(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(s) {
            return Some(bytes);
        }
    }
    None
}

fn parse_vmess(link: &str) -> Result<Server> {
    let body = &link["vmess://".len()..];
    // XTLS-style URL form: vmess://uuid@host:port?...
    if body.contains('@')
        && !body.contains('"')
        && let Ok(p) = parse_url_like(link)
    {
        {
            let name = p
                .name
                .clone()
                .unwrap_or_else(|| Server::default_name(&p.host, p.port));
            return Ok(Server {
                name,
                protocol: Protocol::Vmess,
                address: p.host.clone(),
                port: p.port,
                auth: Auth::Vmess {
                    uuid: p.userinfo.clone(),
                    alter_id: 0,
                    security: opt(&p.query, "encryption").unwrap_or_else(default_vmess_security),
                },
                transport: transport_from_query(&p.query),
                security: security_from_query(&p.query, false)?,
            });
        }
    }
    let bytes = decode_base64_lenient(body)
        .ok_or_else(|| Error::MalformedLink("vmess body is not base64".into()))?;
    let j: VmessJson = serde_json::from_slice(&bytes)
        .map_err(|e| Error::MalformedLink(format!("vmess json: {e}")))?;
    if j.add.is_empty() || j.id.is_empty() {
        return Err(Error::MalformedLink(
            "vmess json without `add` or `id`".into(),
        ));
    }
    let port = value_to_u32(&j.port)
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| Error::MalformedLink("vmess json without valid port".into()))?;
    let non_empty = |s: &str| {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.trim().to_owned())
        }
    };
    let header_type = non_empty(&j.header_type).filter(|h| h != "none");
    let transport = match j.net.to_ascii_lowercase().as_str() {
        "ws" => Transport::Ws {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
        },
        "grpc" | "gun" => Transport::Grpc {
            service_name: non_empty(&j.path),
            mode: None,
        },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
            mode: None,
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
        },
        "kcp" => Transport::Kcp {
            seed: non_empty(&j.path),
            header_type,
        },
        _ => Transport::Tcp { header_type },
    };
    let security = if j.tls.eq_ignore_ascii_case("tls") {
        Security::Tls {
            sni: non_empty(&j.sni),
            alpn: non_empty(&j.alpn)
                .map(|a| a.split(',').map(|s| s.trim().to_owned()).collect())
                .unwrap_or_default(),
            fingerprint: non_empty(&j.fp),
            allow_insecure: false,
        }
    } else {
        Security::None
    };
    Ok(Server {
        name: non_empty(&j.ps).unwrap_or_else(|| Server::default_name(&j.add, port)),
        protocol: Protocol::Vmess,
        address: j.add.clone(),
        port,
        auth: Auth::Vmess {
            uuid: j.id.clone(),
            alter_id: value_to_u32(&j.aid).unwrap_or(0),
            security: non_empty(&j.scy).unwrap_or_else(default_vmess_security),
        },
        transport,
        security,
    })
}

// ---------------------------------------------------------------------------
// Trojan

fn parse_trojan(link: &str) -> Result<Server> {
    let p = parse_url_like(link)?;
    if p.userinfo.is_empty() {
        return Err(Error::MalformedLink("trojan link without password".into()));
    }
    let name = p
        .name
        .clone()
        .unwrap_or_else(|| Server::default_name(&p.host, p.port));
    Ok(Server {
        name,
        protocol: Protocol::Trojan,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Trojan {
            password: p.userinfo.clone(),
        },
        transport: transport_from_query(&p.query),
        security: security_from_query(&p.query, true)?,
    })
}

// ---------------------------------------------------------------------------
// Shadowsocks

fn parse_shadowsocks(link: &str) -> Result<Server> {
    let body = &link["ss://".len()..];
    // Legacy: ss://base64(method:password@host:port)#name
    if !body.contains('@') {
        let (b64, frag) = body.split_once('#').unwrap_or((body, ""));
        let bytes = decode_base64_lenient(b64)
            .ok_or_else(|| Error::MalformedLink("legacy ss link is not base64".into()))?;
        let inner = String::from_utf8_lossy(&bytes).into_owned();
        let rebuilt = format!("ss://{inner}#{frag}");
        return parse_shadowsocks(&rebuilt);
    }
    let p = parse_url_like(link)?;
    // SIP002: userinfo is base64url(method:password) or plain method:password (2022).
    let userinfo = if p.userinfo.contains(':') {
        p.userinfo.clone()
    } else {
        decode_base64_lenient(&p.userinfo)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .ok_or_else(|| {
                Error::MalformedLink("ss userinfo is neither base64 nor method:password".into())
            })?
    };
    let (method, password) = userinfo
        .split_once(':')
        .ok_or_else(|| Error::MalformedLink("ss userinfo without `method:password`".into()))?;
    if opt(&p.query, "plugin").is_some() {
        tracing::warn!("ss link uses a plugin, which is not supported; ignoring it");
    }
    let name = p
        .name
        .clone()
        .unwrap_or_else(|| Server::default_name(&p.host, p.port));
    Ok(Server {
        name,
        protocol: Protocol::Shadowsocks,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Shadowsocks {
            method: method.to_owned(),
            password: password.to_owned(),
        },
        transport: Transport::Tcp { header_type: None },
        security: Security::None,
    })
}

// ---------------------------------------------------------------------------
// Serialising

/// Produces a share link for the server.
pub fn to_link(server: &Server) -> String {
    match (&server.protocol, &server.auth) {
        (Protocol::Vless, Auth::Vless { uuid, flow }) => {
            let mut params = vec![("encryption".to_owned(), "none".to_owned())];
            if let Some(flow) = flow {
                params.push(("flow".into(), flow.clone()));
            }
            push_security_params(&server.security, &mut params);
            push_transport_params(&server.transport, &mut params);
            build_url_like("vless", uuid, server, params)
        }
        (Protocol::Trojan, Auth::Trojan { password }) => {
            let mut params = Vec::new();
            push_security_params(&server.security, &mut params);
            push_transport_params(&server.transport, &mut params);
            build_url_like("trojan", password, server, params)
        }
        (Protocol::Shadowsocks, Auth::Shadowsocks { method, password }) => {
            let userinfo = if method.starts_with("2022-") {
                format!("{method}:{password}")
            } else {
                URL_SAFE_NO_PAD.encode(format!("{method}:{password}"))
            };
            let host = if server.address.contains(':') {
                format!("[{}]", server.address)
            } else {
                server.address.clone()
            };
            let userinfo = if method.starts_with("2022-") {
                encode(&userinfo)
            } else {
                userinfo
            };
            format!(
                "ss://{userinfo}@{host}:{}#{}",
                server.port,
                encode(&server.name)
            )
        }
        (
            Protocol::Vmess,
            Auth::Vmess {
                uuid,
                alter_id,
                security,
            },
        ) => {
            let (net, host, path, header_type) = match &server.transport {
                Transport::Tcp { header_type } => ("tcp", None, None, header_type.clone()),
                Transport::Ws { path, host } => ("ws", host.clone(), path.clone(), None),
                Transport::Grpc { service_name, .. } => ("grpc", None, service_name.clone(), None),
                Transport::Xhttp { path, host, .. } => ("xhttp", host.clone(), path.clone(), None),
                Transport::HttpUpgrade { path, host } => {
                    ("httpupgrade", host.clone(), path.clone(), None)
                }
                Transport::Kcp { seed, header_type } => {
                    ("kcp", None, seed.clone(), header_type.clone())
                }
            };
            let (tls, sni, alpn, fp) = match &server.security {
                Security::Tls {
                    sni,
                    alpn,
                    fingerprint,
                    ..
                } => (
                    "tls",
                    sni.clone(),
                    Some(alpn.join(",")),
                    fingerprint.clone(),
                ),
                _ => ("", None, None, None),
            };
            let j = VmessJson {
                v: serde_json::Value::String("2".into()),
                ps: server.name.clone(),
                add: server.address.clone(),
                port: serde_json::Value::String(server.port.to_string()),
                id: uuid.clone(),
                aid: serde_json::Value::String(alter_id.to_string()),
                scy: security.clone(),
                net: net.into(),
                header_type: header_type.unwrap_or_else(|| "none".into()),
                host: host.unwrap_or_default(),
                path: path.unwrap_or_default(),
                tls: tls.into(),
                sni: sni.unwrap_or_default(),
                alpn: alpn.unwrap_or_default(),
                fp: fp.unwrap_or_default(),
            };
            let json = serde_json::to_string(&j).expect("vmess json is serialisable");
            format!("vmess://{}", STANDARD.encode(json))
        }
        _ => unreachable!("protocol/auth mismatch"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c";

    #[test]
    fn vless_reality_vision() {
        let link = format!(
            "vless://{UUID}@example.org:443?flow=xtls-rprx-vision&fp=random&pbk=RApWyxh62PUorKsccEnjo-KVi3OWKOtsyEhq5xgk9CU&security=reality&sid=95&sni=www.yahoo.com&spx=%2F23b6910affbb7a5&type=tcp#Reality-Test"
        );
        let s = parse_link(&link).unwrap();
        assert_eq!(s.name, "Reality-Test");
        assert_eq!(s.protocol, Protocol::Vless);
        assert_eq!(s.address, "example.org");
        assert_eq!(s.port, 443);
        assert_eq!(
            s.auth,
            Auth::Vless {
                uuid: UUID.into(),
                flow: Some("xtls-rprx-vision".into())
            }
        );
        assert_eq!(s.transport, Transport::Tcp { header_type: None });
        assert_eq!(
            s.security,
            Security::Reality {
                sni: Some("www.yahoo.com".into()),
                fingerprint: Some("random".into()),
                public_key: "RApWyxh62PUorKsccEnjo-KVi3OWKOtsyEhq5xgk9CU".into(),
                short_id: Some("95".into()),
                spider_x: Some("/23b6910affbb7a5".into()),
            }
        );
        // Round trip.
        let again = parse_link(&to_link(&s)).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn vless_ws_tls_with_ipv6_and_alpn() {
        let link = format!(
            "vless://{UUID}@[2001:db8::1]:8443?type=ws&path=%2Fchat&host=cdn.example.com&security=tls&sni=cdn.example.com&alpn=h2,http/1.1&fp=chrome&allowInsecure=1#WS%20Server"
        );
        let s = parse_link(&link).unwrap();
        assert_eq!(s.name, "WS Server");
        assert_eq!(s.address, "2001:db8::1");
        assert_eq!(
            s.transport,
            Transport::Ws {
                path: Some("/chat".into()),
                host: Some("cdn.example.com".into())
            }
        );
        assert_eq!(
            s.security,
            Security::Tls {
                sni: Some("cdn.example.com".into()),
                alpn: vec!["h2".into(), "http/1.1".into()],
                fingerprint: Some("chrome".into()),
                allow_insecure: true,
            }
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn vless_without_name_gets_default() {
        let s = parse_link(&format!(
            "vless://{UUID}@1.2.3.4:80?security=none&type=grpc&serviceName=svc"
        ))
        .unwrap();
        assert_eq!(s.name, "1.2.3.4:80");
        assert_eq!(
            s.transport,
            Transport::Grpc {
                service_name: Some("svc".into()),
                mode: None
            }
        );
    }

    #[test]
    fn vless_reality_requires_pbk() {
        let err = parse_link(&format!("vless://{UUID}@h:443?security=reality")).unwrap_err();
        assert!(matches!(err, Error::MalformedLink(_)));
    }

    #[test]
    fn vmess_base64_json_roundtrip() {
        let json = format!(
            r#"{{"v":"2","ps":"VMess WS","add":"vm.example.com","port":"443","id":"{UUID}","aid":"0","scy":"auto","net":"ws","type":"none","host":"vm.example.com","path":"/ws","tls":"tls","sni":"vm.example.com","alpn":"","fp":"chrome"}}"#
        );
        let link = format!("vmess://{}", STANDARD.encode(json));
        let s = parse_link(&link).unwrap();
        assert_eq!(s.protocol, Protocol::Vmess);
        assert_eq!(s.name, "VMess WS");
        assert_eq!(s.port, 443);
        assert_eq!(
            s.auth,
            Auth::Vmess {
                uuid: UUID.into(),
                alter_id: 0,
                security: "auto".into()
            }
        );
        assert!(matches!(s.transport, Transport::Ws { .. }));
        assert!(matches!(s.security, Security::Tls { .. }));
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn vmess_numeric_port_and_aid() {
        let json =
            format!(r#"{{"add":"h.example","port":8080,"id":"{UUID}","aid":64,"net":"tcp"}}"#);
        let s = parse_link(&format!("vmess://{}", STANDARD.encode(json))).unwrap();
        assert_eq!(s.port, 8080);
        assert!(matches!(s.auth, Auth::Vmess { alter_id: 64, .. }));
        assert_eq!(s.security, Security::None);
    }

    #[test]
    fn trojan_defaults_to_tls() {
        let s = parse_link("trojan://p%40ss@tj.example.com:443?sni=tj.example.com#Trojan").unwrap();
        assert_eq!(
            s.auth,
            Auth::Trojan {
                password: "p@ss".into()
            }
        );
        assert!(
            matches!(s.security, Security::Tls { ref sni, .. } if sni.as_deref() == Some("tj.example.com"))
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn shadowsocks_sip002_base64() {
        let userinfo = URL_SAFE_NO_PAD.encode("aes-256-gcm:secret");
        let s = parse_link(&format!("ss://{userinfo}@ss.example.com:8388#SS%20Node")).unwrap();
        assert_eq!(s.name, "SS Node");
        assert_eq!(
            s.auth,
            Auth::Shadowsocks {
                method: "aes-256-gcm".into(),
                password: "secret".into()
            }
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn shadowsocks_2022_plain() {
        let s = parse_link("ss://2022-blake3-aes-128-gcm:YctPZ6U7xPPcU%2Bgp3u%2B0tx%2FtRizJN9K8y%2BuKlW2qjlI%3D@h.example:443#SS2022").unwrap();
        assert_eq!(
            s.auth,
            Auth::Shadowsocks {
                method: "2022-blake3-aes-128-gcm".into(),
                password: "YctPZ6U7xPPcU+gp3u+0tx/tRizJN9K8y+uKlW2qjlI=".into()
            }
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn shadowsocks_legacy_whole_base64() {
        let inner = STANDARD.encode("chacha20-ietf-poly1305:pw@legacy.example:9000");
        let s = parse_link(&format!("ss://{inner}#Legacy")).unwrap();
        assert_eq!(s.address, "legacy.example");
        assert_eq!(s.port, 9000);
        assert_eq!(s.name, "Legacy");
    }

    #[test]
    fn parse_many_skips_noise_and_reports_failures() {
        let text = format!(
            "hello\nvless://{UUID}@a.example:443?security=none#A\n\nvless://broken\nss://notbase64@h:1\n"
        );
        let (ok, failed) = parse_many(&text);
        assert_eq!(ok.len(), 1);
        assert_eq!(failed.len(), 2);
    }

    #[test]
    fn unsupported_scheme() {
        assert!(matches!(
            parse_link("hysteria2://x@h:1"),
            Err(Error::UnsupportedScheme(_))
        ));
        assert!(!looks_like_link("https://example.com"));
        assert!(looks_like_link("  VLESS://x"));
    }
}
