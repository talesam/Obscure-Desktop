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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Vless,
    Vmess,
    Trojan,
    Shadowsocks,
    Wireguard,
    Socks,
    Http,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Vless => "VLESS",
            Protocol::Vmess => "VMess",
            Protocol::Trojan => "Trojan",
            Protocol::Shadowsocks => "Shadowsocks",
            Protocol::Wireguard => "WireGuard",
            Protocol::Socks => "SOCKS",
            Protocol::Http => "HTTP",
        }
    }
}

/// Protocol-specific credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    Wireguard {
        private_key: String,
        public_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preshared_key: Option<String>,
        /// Local tunnel addresses, e.g. `172.16.0.2/32`.
        #[serde(default)]
        address: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        reserved: Vec<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mtu: Option<u32>,
    },
    /// SOCKS5 or HTTP upstream proxy with optional credentials.
    UserPass {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pass: Option<String>,
    },
}

fn default_vmess_security() -> String {
    "auto".into()
}

/// Stream transport.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        /// `extra` share-link parameter: a JSON object (stored minified)
        /// merged by Xray into `xhttpSettings`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
        /// Kept for share-link round trips only: Xray 26 removed
        /// `allowInsecure`, so it never reaches the generated config.
        #[serde(default)]
        allow_insecure: bool,
        /// `pcs`: SHA-256 of the server certificate (hex, comma-separated),
        /// Xray's `pinnedPeerCertSha256`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pinned_peer_cert_sha256: Option<String>,
        /// `vcn`: name checked against the certificate instead of the SNI,
        /// Xray's `verifyPeerCertByName`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verify_peer_cert_by_name: Option<String>,
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
        /// `pqv`: ML-DSA-65 public key, Xray's `mldsa65Verify`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mldsa65_verify: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// The address as written in URLs and `host:port` pairs: IPv6
    /// literals are wrapped in brackets.
    pub fn url_host(&self) -> String {
        if self.address.contains(':') {
            format!("[{}]", self.address)
        } else {
            self.address.clone()
        }
    }

    /// Checks invariants the parsers guarantee but a hand-edited
    /// `profiles.json` may break: credentials matching the protocol and a
    /// well-formed XHTTP `extra`.
    pub fn validate(&self) -> Result<()> {
        let consistent = matches!(
            (self.protocol, &self.auth),
            (Protocol::Vless, Auth::Vless { .. })
                | (Protocol::Vmess, Auth::Vmess { .. })
                | (Protocol::Trojan, Auth::Trojan { .. })
                | (Protocol::Shadowsocks, Auth::Shadowsocks { .. })
                | (Protocol::Wireguard, Auth::Wireguard { .. })
                | (Protocol::Socks | Protocol::Http, Auth::UserPass { .. })
        );
        if !consistent {
            return Err(Error::MalformedLink(format!(
                "{} server with credentials of another protocol",
                self.protocol.label()
            )));
        }
        if let Transport::Xhttp {
            extra: Some(extra), ..
        } = &self.transport
        {
            xhttp_extra_object(extra)?;
        }
        Ok(())
    }
}

/// Parses an XHTTP `extra` value, which must be a JSON object.
pub(crate) fn xhttp_extra_object(
    extra: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_str(extra) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        _ => Err(Error::MalformedLink(
            "xhttp `extra` is not a JSON object".into(),
        )),
    }
}

/// Minified form of an XHTTP `extra` object, so equal settings compare equal.
fn canonical_xhttp_extra(extra: &str) -> Result<String> {
    Ok(serde_json::Value::Object(xhttp_extra_object(extra)?).to_string())
}

/// Returns `true` if the text looks like a share link. This includes
/// proxy schemes Obscure cannot use yet (Hysteria 2, TUIC, ShadowsocksR), so
/// that an import reports them as unsupported instead of ignoring them.
pub fn looks_like_link(text: &str) -> bool {
    let t = text.trim();
    [
        "vless://",
        "vmess://",
        "trojan://",
        "ss://",
        "wireguard://",
        "wg://",
        "socks://",
        "socks5://",
        "hysteria2://",
        "hy2://",
        "tuic://",
        "ssr://",
    ]
    .iter()
    .any(|p| t.len() > p.len() && t.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)))
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
        "wireguard" | "wg" => parse_wireguard(link),
        "socks" | "socks5" => parse_socks(link),
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
    let query = parse_query(url.query().unwrap_or_default());
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

impl Parsed {
    /// The link's name, or `host:port` when it carries none.
    fn name(&self) -> String {
        name_or_default(self.name.clone(), &self.host, self.port)
    }
}

fn name_or_default(name: Option<String>, host: &str, port: u16) -> String {
    name.unwrap_or_else(|| Server::default_name(host, port))
}

/// Splits a query string into decoded pairs. Unlike form decoding
/// (`Url::query_pairs`), `+` stays a literal plus: share links carry base64
/// keys (`publickey=ab+cd/ef=`) and paths where it is significant.
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// Splits a comma-separated list (`alpn=h2,http/1.1`), dropping empty items.
fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
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

fn unsupported_transport(kind: &str) -> Error {
    Error::MalformedLink(format!("unsupported transport `{kind}`"))
}

fn transport_from_query(q: &HashMap<String, String>) -> Result<Transport> {
    let kind = opt(q, "type").unwrap_or_else(|| "tcp".into());
    let header_type = opt(q, "headerType").filter(|h| h != "none");
    Ok(match kind.to_ascii_lowercase().as_str() {
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
            extra: opt(q, "extra")
                .map(|e| canonical_xhttp_extra(&e))
                .transpose()?,
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: opt(q, "path"),
            host: opt(q, "host"),
        },
        "kcp" | "mkcp" => Transport::Kcp {
            seed: opt(q, "seed"),
            header_type,
        },
        "tcp" | "raw" => Transport::Tcp { header_type },
        other => return Err(unsupported_transport(other)),
    })
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
            alpn: opt(q, "alpn").map(|a| split_list(&a)).unwrap_or_default(),
            fingerprint: opt(q, "fp"),
            allow_insecure: is_truthy(
                opt(q, "allowInsecure")
                    .or_else(|| opt(q, "insecure"))
                    .as_deref(),
            ),
            pinned_peer_cert_sha256: opt(q, "pcs"),
            verify_peer_cert_by_name: opt(q, "vcn"),
        },
        "reality" => Security::Reality {
            sni: opt(q, "sni"),
            fingerprint: opt(q, "fp"),
            public_key: opt(q, "pbk")
                .ok_or_else(|| Error::MalformedLink("reality link without `pbk`".into()))?,
            short_id: opt(q, "sid"),
            spider_x: opt(q, "spx"),
            mldsa65_verify: opt(q, "pqv"),
        },
        other => return Err(Error::MalformedLink(format!("unknown security `{other}`"))),
    })
}

fn is_truthy(v: Option<&str>) -> bool {
    matches!(v.map(str::trim), Some("1") | Some("true") | Some("True"))
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
        Transport::Xhttp {
            path,
            host,
            mode,
            extra,
        } => {
            add(params, "path", path);
            add(params, "host", host);
            add(params, "mode", mode);
            add(params, "extra", extra);
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
            pinned_peer_cert_sha256,
            verify_peer_cert_by_name,
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
            add(params, "pcs", pinned_peer_cert_sha256);
            add(params, "vcn", verify_peer_cert_by_name);
        }
        Security::Reality {
            sni,
            fingerprint,
            public_key,
            short_id,
            spider_x,
            mldsa65_verify,
        } => {
            params.push(("security".into(), "reality".into()));
            add(params, "sni", sni);
            add(params, "fp", fingerprint);
            params.push(("pbk".into(), public_key.clone()));
            add(params, "sid", short_id);
            add(params, "spx", spider_x);
            add(params, "pqv", mldsa65_verify);
        }
    }
}

fn build_url_like(
    scheme: &str,
    userinfo: &str,
    server: &Server,
    params: Vec<(String, String)>,
) -> String {
    let host = server.url_host();
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
    let mut p = parse_url_like(link)?;
    // A UUID never holds whitespace; stray spaces come from copy and paste.
    p.userinfo = p.userinfo.trim().to_owned();
    if p.userinfo.is_empty() {
        return Err(Error::MalformedLink("vless link without uuid".into()));
    }
    let encryption = opt(&p.query, "encryption").unwrap_or_else(|| "none".into());
    if encryption != "none" {
        return Err(Error::MalformedLink(format!(
            "unsupported vless encryption `{encryption}`"
        )));
    }
    let name = p.name();
    Ok(Server {
        name,
        protocol: Protocol::Vless,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Vless {
            uuid: p.userinfo.clone(),
            flow: opt(&p.query, "flow"),
        },
        transport: transport_from_query(&p.query)?,
        security: security_from_query(&p.query, false)?,
    })
}

// ---------------------------------------------------------------------------
// VMess

/// v2rayN-style VMess JSON. Generators disagree on types (`"port":"443"` or
/// `443`, `"host":null`, `"alpn":["h2"]`), so every field is read leniently.
#[derive(Debug, Default, Serialize, Deserialize)]
struct VmessJson {
    #[serde(default)]
    v: serde_json::Value,
    #[serde(default, deserialize_with = "lenient_string")]
    ps: String,
    #[serde(default, deserialize_with = "lenient_string")]
    add: String,
    #[serde(default)]
    port: serde_json::Value,
    #[serde(default, deserialize_with = "lenient_string")]
    id: String,
    #[serde(default)]
    aid: serde_json::Value,
    #[serde(default, deserialize_with = "lenient_string")]
    scy: String,
    #[serde(default, deserialize_with = "lenient_string")]
    net: String,
    /// TCP/mKCP header type, or the gRPC/XHTTP mode (v2rayN convention).
    #[serde(default, rename = "type", deserialize_with = "lenient_string")]
    header_type: String,
    #[serde(default, deserialize_with = "lenient_string")]
    host: String,
    #[serde(default, deserialize_with = "lenient_string")]
    path: String,
    #[serde(default, deserialize_with = "lenient_string")]
    tls: String,
    #[serde(default, deserialize_with = "lenient_string")]
    sni: String,
    #[serde(default, deserialize_with = "lenient_string")]
    alpn: String,
    #[serde(default, deserialize_with = "lenient_string")]
    fp: String,
    #[serde(
        default,
        rename = "allowInsecure",
        alias = "insecure",
        deserialize_with = "lenient_string",
        skip_serializing_if = "String::is_empty"
    )]
    allow_insecure: String,
}

/// Reads any JSON scalar as a string: `null` becomes empty, numbers and
/// booleans their text, and an array of strings a comma-separated list.
fn lenient_string<'de, D>(d: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde_json::Value;
    Ok(match Option::<Value>::deserialize(d)? {
        None | Some(Value::Null) | Some(Value::Object(_)) => String::new(),
        Some(Value::String(s)) => s,
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(","),
    })
}

fn value_to_u32(v: &serde_json::Value) -> Option<u32> {
    match v {
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f <= f64::from(u32::MAX))
                    .map(|f| f as u64)
            })
            .and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Decodes base64 in any of the alphabets found in the wild (standard or
/// URL-safe, padded or not). Whitespace (line wrapping) is ignored.
pub(crate) fn decode_base64_lenient(s: &str) -> Option<Vec<u8>> {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .into_iter()
        .find_map(|engine| engine.decode(&compact).ok())
}

fn parse_vmess(link: &str) -> Result<Server> {
    let body = &link["vmess://".len()..];
    // XTLS-style URL form: vmess://uuid@host:port?...
    if body.contains('@')
        && !body.contains('"')
        && let Ok(mut p) = parse_url_like(link)
    {
        p.userinfo = p.userinfo.trim().to_owned();
        if p.userinfo.is_empty() {
            return Err(Error::MalformedLink("vmess link without uuid".into()));
        }
        return Ok(Server {
            name: p.name(),
            protocol: Protocol::Vmess,
            address: p.host.clone(),
            port: p.port,
            auth: Auth::Vmess {
                uuid: p.userinfo.clone(),
                // Not part of the standard (AEAD only); written by our own
                // export so that legacy servers survive a round trip.
                alter_id: opt(&p.query, "aid")
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(0),
                security: opt(&p.query, "encryption").unwrap_or_else(default_vmess_security),
            },
            transport: transport_from_query(&p.query)?,
            security: security_from_query(&p.query, false)?,
        });
    }
    let bytes = decode_base64_lenient(body)
        .ok_or_else(|| Error::MalformedLink("vmess body is not base64".into()))?;
    let j: VmessJson = serde_json::from_slice(&bytes)
        .map_err(|e| Error::MalformedLink(format!("vmess json: {e}")))?;
    let non_empty = |s: &str| Some(s.trim().to_owned()).filter(|s| !s.is_empty());
    let (Some(address), Some(uuid)) = (non_empty(&j.add), non_empty(&j.id)) else {
        return Err(Error::MalformedLink(
            "vmess json without `add` or `id`".into(),
        ));
    };
    let port = value_to_u32(&j.port)
        .and_then(|p| u16::try_from(p).ok())
        .ok_or_else(|| Error::MalformedLink("vmess json without valid port".into()))?;
    let header_type = non_empty(&j.header_type).filter(|h| h != "none");
    let transport = match j.net.trim().to_ascii_lowercase().as_str() {
        "ws" => Transport::Ws {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
        },
        "grpc" | "gun" => Transport::Grpc {
            service_name: non_empty(&j.path),
            mode: header_type,
        },
        "xhttp" | "splithttp" => Transport::Xhttp {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
            mode: header_type,
            extra: None,
        },
        "httpupgrade" => Transport::HttpUpgrade {
            path: non_empty(&j.path),
            host: non_empty(&j.host),
        },
        "kcp" | "mkcp" => Transport::Kcp {
            seed: non_empty(&j.path),
            header_type,
        },
        "" | "tcp" | "raw" => Transport::Tcp { header_type },
        other => return Err(unsupported_transport(other)),
    };
    let security = match j.tls.trim().to_ascii_lowercase().as_str() {
        "tls" | "xtls" => Security::Tls {
            sni: non_empty(&j.sni),
            alpn: split_list(&j.alpn),
            fingerprint: non_empty(&j.fp),
            allow_insecure: is_truthy(Some(&j.allow_insecure)),
            pinned_peer_cert_sha256: None,
            verify_peer_cert_by_name: None,
        },
        // REALITY needs `pbk`, which the JSON form has no field for.
        "reality" => {
            return Err(Error::MalformedLink(
                "vmess json cannot describe reality".into(),
            ));
        }
        _ => Security::None,
    };
    Ok(Server {
        name: name_or_default(non_empty(&j.ps), &address, port),
        protocol: Protocol::Vmess,
        address,
        port,
        auth: Auth::Vmess {
            uuid,
            alter_id: value_to_u32(&j.aid).unwrap_or(0),
            security: non_empty(&j.scy).unwrap_or_else(default_vmess_security),
        },
        transport,
        security,
    })
}

/// VMess share link. The v2rayN JSON form is what most clients read, so it
/// is preferred; settings it cannot carry (REALITY, certificate pinning,
/// XHTTP `extra`) switch to the XTLS URL form, which keeps them all.
fn vmess_link(server: &Server, uuid: &str, alter_id: u32, security: &str) -> String {
    let json_lossy = match &server.security {
        Security::Reality { .. } => true,
        Security::Tls {
            pinned_peer_cert_sha256,
            verify_peer_cert_by_name,
            ..
        } => pinned_peer_cert_sha256.is_some() || verify_peer_cert_by_name.is_some(),
        Security::None => false,
    } || matches!(server.transport, Transport::Xhttp { extra: Some(_), .. });
    if json_lossy {
        let mut params = vec![("encryption".to_owned(), security.to_owned())];
        if alter_id != 0 {
            params.push(("aid".into(), alter_id.to_string()));
        }
        push_security_params(&server.security, &mut params);
        push_transport_params(&server.transport, &mut params);
        return build_url_like("vmess", uuid, server, params);
    }
    let (net, host, path, header_type) = match &server.transport {
        Transport::Tcp { header_type } => ("tcp", None, None, header_type.clone()),
        Transport::Ws { path, host } => ("ws", host.clone(), path.clone(), None),
        Transport::Grpc { service_name, mode } => {
            ("grpc", None, service_name.clone(), mode.clone())
        }
        Transport::Xhttp {
            path, host, mode, ..
        } => ("xhttp", host.clone(), path.clone(), mode.clone()),
        Transport::HttpUpgrade { path, host } => ("httpupgrade", host.clone(), path.clone(), None),
        Transport::Kcp { seed, header_type } => ("kcp", None, seed.clone(), header_type.clone()),
    };
    let (tls, sni, alpn, fp, insecure) = match &server.security {
        Security::Tls {
            sni,
            alpn,
            fingerprint,
            allow_insecure,
            ..
        } => (
            "tls",
            sni.clone(),
            alpn.join(","),
            fingerprint.clone(),
            *allow_insecure,
        ),
        _ => ("", None, String::new(), None, false),
    };
    let j = VmessJson {
        v: serde_json::Value::String("2".into()),
        ps: server.name.clone(),
        add: server.address.clone(),
        port: serde_json::Value::String(server.port.to_string()),
        id: uuid.to_owned(),
        aid: serde_json::Value::String(alter_id.to_string()),
        scy: security.to_owned(),
        net: net.into(),
        header_type: header_type.unwrap_or_else(|| "none".into()),
        host: host.unwrap_or_default(),
        path: path.unwrap_or_default(),
        tls: tls.into(),
        sni: sni.unwrap_or_default(),
        alpn,
        fp: fp.unwrap_or_default(),
        allow_insecure: if insecure { "1".into() } else { String::new() },
    };
    // Serialising a struct of strings and JSON values cannot fail.
    let json = serde_json::to_string(&j).unwrap_or_default();
    format!("vmess://{}", STANDARD.encode(json))
}

// ---------------------------------------------------------------------------
// Trojan

fn parse_trojan(link: &str) -> Result<Server> {
    let p = parse_url_like(link)?;
    if p.userinfo.is_empty() {
        return Err(Error::MalformedLink("trojan link without password".into()));
    }
    let name = p.name();
    Ok(Server {
        name,
        protocol: Protocol::Trojan,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Trojan {
            password: p.userinfo.clone(),
        },
        transport: transport_from_query(&p.query)?,
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
        // Decoding once is enough; anything else would recurse forever on
        // an empty body (`ss://#x`).
        if !inner.contains('@') {
            return Err(Error::MalformedLink(
                "legacy ss link without `method:password@host:port`".into(),
            ));
        }
        return parse_shadowsocks_sip002(&format!("ss://{inner}#{frag}"));
    }
    parse_shadowsocks_sip002(link)
}

fn parse_shadowsocks_sip002(link: &str) -> Result<Server> {
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
    // Without its plugin (obfs, v2ray-plugin…) the server cannot be reached.
    if let Some(plugin) = opt(&p.query, "plugin") {
        let name = plugin.split(';').next().unwrap_or_default().trim();
        return Err(Error::MalformedLink(format!(
            "unsupported shadowsocks plugin `{name}`"
        )));
    }
    let name = p.name();
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
// WireGuard (v2rayN / Hiddify style link)

fn parse_wireguard(link: &str) -> Result<Server> {
    let p = parse_url_like(link)?;
    if p.userinfo.is_empty() {
        return Err(Error::MalformedLink(
            "wireguard link without private key".into(),
        ));
    }
    let public_key = opt(&p.query, "publickey")
        .or_else(|| opt(&p.query, "pbk"))
        .ok_or_else(|| Error::MalformedLink("wireguard link without `publickey`".into()))?;
    let list = |key: &str| -> Vec<String> {
        opt(&p.query, key)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let reserved: Vec<u8> = list("reserved")
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let name = p.name();
    Ok(Server {
        name,
        protocol: Protocol::Wireguard,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::Wireguard {
            private_key: p.userinfo.clone(),
            public_key,
            preshared_key: opt(&p.query, "presharedkey").or_else(|| opt(&p.query, "psk")),
            address: list("address"),
            reserved,
            mtu: opt(&p.query, "mtu").and_then(|m| m.parse().ok()),
        },
        transport: Transport::Tcp { header_type: None },
        security: Security::None,
    })
}

// ---------------------------------------------------------------------------
// SOCKS5 upstream

fn parse_socks(link: &str) -> Result<Server> {
    let p = parse_url_like(link)?;
    let (user, pass) = if p.userinfo.is_empty() {
        (None, None)
    } else {
        // Plain `user:pass` or base64(user:pass), as some clients emit. A
        // bare user name may happen to be valid base64 (`abc`), so the
        // decoded form only wins when it is text holding `user:pass`.
        let raw = if p.userinfo.contains(':') {
            p.userinfo.clone()
        } else {
            decode_base64_lenient(&p.userinfo)
                .and_then(|b| String::from_utf8(b).ok())
                .filter(|s| s.contains(':'))
                .unwrap_or_else(|| p.userinfo.clone())
        };
        match raw.split_once(':') {
            Some((u, pw)) => (Some(u.to_owned()), Some(pw.to_owned())),
            None => (Some(raw), None),
        }
    };
    let name = p.name();
    Ok(Server {
        name,
        protocol: Protocol::Socks,
        address: p.host.clone(),
        port: p.port,
        auth: Auth::UserPass { user, pass },
        transport: Transport::Tcp { header_type: None },
        security: security_from_query(&p.query, false)?,
    })
}

// ---------------------------------------------------------------------------
// Serialising

/// Produces a share link for the server, or an error when the server is
/// inconsistent (see [`Server::validate`]).
pub fn try_to_link(server: &Server) -> Result<String> {
    server.validate()?;
    Ok(match (&server.protocol, &server.auth) {
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
                encode(&format!("{method}:{password}"))
            } else {
                URL_SAFE_NO_PAD.encode(format!("{method}:{password}"))
            };
            let host = server.url_host();
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
        ) => vmess_link(server, uuid, *alter_id, security),
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
            let mut params = vec![("publickey".to_owned(), public_key.clone())];
            if !address.is_empty() {
                params.push(("address".into(), address.join(",")));
            }
            if !reserved.is_empty() {
                params.push((
                    "reserved".into(),
                    reserved
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ));
            }
            if let Some(psk) = preshared_key {
                params.push(("presharedkey".into(), psk.clone()));
            }
            if let Some(mtu) = mtu {
                params.push(("mtu".into(), mtu.to_string()));
            }
            build_url_like("wireguard", private_key, server, params)
        }
        (Protocol::Socks | Protocol::Http, Auth::UserPass { user, pass }) => {
            let userinfo = match (user, pass) {
                (Some(u), Some(p)) => format!("{u}:{p}"),
                (Some(u), None) => u.clone(),
                _ => String::new(),
            };
            let scheme = if server.protocol == Protocol::Socks {
                "socks"
            } else {
                "http"
            };
            let mut params = Vec::new();
            if server.security != Security::None {
                push_security_params(&server.security, &mut params);
            }
            let host = server.url_host();
            let mut out = format!("{scheme}://");
            if !userinfo.is_empty() {
                out.push_str(&encode(&userinfo));
                out.push('@');
            }
            out.push_str(&format!("{host}:{}", server.port));
            if !params.is_empty() {
                out.push('?');
                out.push_str(
                    &params
                        .iter()
                        .map(|(k, v)| format!("{k}={}", encode(v)))
                        .collect::<Vec<_>>()
                        .join("&"),
                );
            }
            out.push('#');
            out.push_str(&encode(&server.name));
            out
        }
        _ => {
            return Err(Error::MalformedLink(
                "protocol and credentials do not match".into(),
            ));
        }
    })
}

/// Produces a share link for the server. An inconsistent server (only
/// possible through a hand-edited profile) yields an empty string.
pub fn to_link(server: &Server) -> String {
    try_to_link(server).unwrap_or_else(|e| {
        tracing::warn!("cannot export server `{}`: {e}", server.name);
        String::new()
    })
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
                mldsa65_verify: None,
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
                pinned_peer_cert_sha256: None,
                verify_peer_cert_by_name: None,
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
    fn wireguard_roundtrip() {
        let s = parse_link("wireguard://cHJpdmF0ZUtleQ%3D%3D@wg.example:51820?publickey=cHVibGlj&address=172.16.0.2/32,fd00::2/128&reserved=1,2,3&mtu=1280#WG").unwrap();
        assert_eq!(s.protocol, Protocol::Wireguard);
        match &s.auth {
            Auth::Wireguard {
                private_key,
                public_key,
                address,
                reserved,
                mtu,
                ..
            } => {
                assert_eq!(private_key, "cHJpdmF0ZUtleQ==");
                assert_eq!(public_key, "cHVibGlj");
                assert_eq!(
                    address,
                    &vec!["172.16.0.2/32".to_owned(), "fd00::2/128".to_owned()]
                );
                assert_eq!(reserved, &vec![1, 2, 3]);
                assert_eq!(*mtu, Some(1280));
            }
            other => panic!("wrong auth {other:?}"),
        }
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn socks_with_and_without_credentials() {
        let s = parse_link("socks://usr:p%40ss@s.example:1080#S").unwrap();
        assert_eq!(s.protocol, Protocol::Socks);
        assert_eq!(
            s.auth,
            Auth::UserPass {
                user: Some("usr".into()),
                pass: Some("p@ss".into())
            }
        );
        let plain = parse_link("socks5://s.example:1080").unwrap();
        assert_eq!(
            plain.auth,
            Auth::UserPass {
                user: None,
                pass: None
            }
        );
        assert_eq!(plain.name, "s.example:1080");
        let b64 = parse_link(&format!(
            "socks://{}@s.example:1080#B",
            STANDARD.encode("u:p")
        ))
        .unwrap();
        assert_eq!(
            b64.auth,
            Auth::UserPass {
                user: Some("u".into()),
                pass: Some("p".into())
            }
        );
        assert_eq!(parse_link(&to_link(&plain)).unwrap(), plain);
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
    fn plus_in_query_values_is_literal() {
        let s = parse_link(
            "wireguard://cHJpdg%3D%3D@wg.example:51820?publickey=ab+cd/ef=&presharedkey=x+y%2Bz=#W",
        )
        .unwrap();
        match &s.auth {
            Auth::Wireguard {
                public_key,
                preshared_key,
                ..
            } => {
                assert_eq!(public_key, "ab+cd/ef=");
                assert_eq!(preshared_key.as_deref(), Some("x+y+z="));
            }
            other => panic!("wrong auth {other:?}"),
        }
        let r = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=reality&pbk=k+1&spx=/a+b&type=ws&path=/p+q%20r"
        ))
        .unwrap();
        assert!(
            matches!(&r.security, Security::Reality { public_key, spider_x, .. }
                if public_key == "k+1" && spider_x.as_deref() == Some("/a+b"))
        );
        assert!(
            matches!(&r.transport, Transport::Ws { path, .. } if path.as_deref() == Some("/p+q r"))
        );
        assert_eq!(parse_link(&to_link(&r)).unwrap(), r);
    }

    #[test]
    fn unknown_transports_are_rejected() {
        for t in ["h2", "http", "quic", "bogus"] {
            let err = parse_link(&format!("vless://{UUID}@h.example:443?type={t}")).unwrap_err();
            assert!(
                matches!(&err, Error::MalformedLink(m) if m.contains(t)),
                "{t}: {err:?}"
            );
        }
        for t in ["tcp", "raw", "TCP"] {
            let s = parse_link(&format!("vless://{UUID}@h.example:443?type={t}")).unwrap();
            assert_eq!(s.transport, Transport::Tcp { header_type: None });
        }
        for net in ["h2", "quic"] {
            let json = format!(r#"{{"add":"h.example","port":443,"id":"{UUID}","net":"{net}"}}"#);
            let err = parse_link(&format!("vmess://{}", STANDARD.encode(json))).unwrap_err();
            assert!(matches!(err, Error::MalformedLink(_)), "{net}");
        }
        let json = format!(r#"{{"add":"h.example","port":443,"id":"{UUID}"}}"#);
        let s = parse_link(&format!("vmess://{}", STANDARD.encode(json))).unwrap();
        assert_eq!(s.transport, Transport::Tcp { header_type: None });
    }

    #[test]
    fn empty_alpn_items_are_dropped() {
        let s = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=tls&alpn=h2,,http/1.1,"
        ))
        .unwrap();
        assert!(
            matches!(&s.security, Security::Tls { alpn, .. } if alpn == &vec!["h2".to_owned(), "http/1.1".to_owned()])
        );
        let json =
            format!(r#"{{"add":"h.example","port":443,"id":"{UUID}","tls":"tls","alpn":"h2,"}}"#);
        let v = parse_link(&format!("vmess://{}", STANDARD.encode(json))).unwrap();
        assert!(
            matches!(&v.security, Security::Tls { alpn, .. } if alpn == &vec!["h2".to_owned()])
        );
    }

    #[test]
    fn unsupported_proxy_schemes_are_reported_not_dropped() {
        let text = format!(
            "vless://{UUID}@a.example:443?security=none#A\nhysteria2://pw@h:443\nhy2://pw@h:443\ntuic://u:p@h:443\nssr://abc\n"
        );
        let (ok, failed) = parse_many(&text);
        assert_eq!(ok.len(), 1);
        assert_eq!(failed.len(), 4);
        assert!(
            failed
                .iter()
                .all(|(_, e)| matches!(e, Error::UnsupportedScheme(_)))
        );
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

    #[test]
    fn non_ascii_text_is_not_a_link() {
        assert!(!looks_like_link("日本語日本語日本語"));
        assert!(!looks_like_link("Привет, это подписка"));
        assert!(!looks_like_link("🚀🚀🚀🚀 servers"));
        let (servers, failed) = parse_many("节点列表 日本語日本語\nvless://u@h:1#n");
        assert_eq!(servers.len(), 1);
        assert!(failed.is_empty());
    }

    fn vmess(json: &str) -> Result<Server> {
        parse_link(&format!("vmess://{}", STANDARD.encode(json)))
    }

    #[test]
    fn mismatched_protocol_and_auth_does_not_panic() {
        let mut s = parse_link(&format!("vless://{UUID}@h.example:443#V")).unwrap();
        assert!(s.validate().is_ok());
        s.auth = Auth::Shadowsocks {
            method: "aes-256-gcm".into(),
            password: "pw".into(),
        };
        assert!(matches!(s.validate(), Err(Error::MalformedLink(_))));
        assert!(try_to_link(&s).is_err());
        assert_eq!(to_link(&s), "");
        // Http with user/pass is consistent even though no link produces it.
        s.protocol = Protocol::Http;
        s.auth = Auth::UserPass {
            user: None,
            pass: None,
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn vmess_json_accepts_null_and_mixed_types() {
        let s = vmess(&format!(
            r#"{{"v":2,"ps":null,"add":"h.example","port":"8443","id":"{UUID}","aid":"16","scy":null,"net":null,"type":null,"host":null,"path":null,"tls":null,"sni":null,"alpn":null,"fp":null}}"#
        ))
        .unwrap();
        assert_eq!(s.port, 8443);
        assert_eq!(s.name, "h.example:8443");
        assert_eq!(
            s.auth,
            Auth::Vmess {
                uuid: UUID.into(),
                alter_id: 16,
                security: "auto".into()
            }
        );
        assert_eq!(s.transport, Transport::Tcp { header_type: None });
        assert_eq!(s.security, Security::None);

        let s = vmess(&format!(
            r#"{{"add":"h.example","port":443.0,"id":"{UUID}","aid":0,"net":"ws","host":"cdn.example","path":"/w","tls":"tls","alpn":["h2","http/1.1"],"sni":123}}"#
        ))
        .unwrap();
        assert_eq!(s.port, 443);
        assert!(matches!(&s.security, Security::Tls { alpn, sni, .. }
            if alpn == &vec!["h2".to_owned(), "http/1.1".to_owned()] && sni.as_deref() == Some("123")));
        // Out-of-range numbers are rejected or ignored, never truncated.
        for port in ["65536", "\"70000\"", "4294967739", "-1", "null", "\"\""] {
            let r = vmess(&format!(r#"{{"add":"h","port":{port},"id":"{UUID}"}}"#));
            assert!(matches!(r, Err(Error::MalformedLink(_))), "port {port}");
        }
        let s = vmess(&format!(
            r#"{{"add":"h","port":1,"id":"{UUID}","aid":4294967297}}"#
        ))
        .unwrap();
        assert!(matches!(s.auth, Auth::Vmess { alter_id: 0, .. }));
        // Blank `add`/`id` are as bad as missing ones.
        assert!(vmess(r#"{"add":" ","port":1,"id":"x"}"#).is_err());
        assert!(vmess(r#"{"add":"h","port":1,"id":null}"#).is_err());
        // REALITY cannot be described by the JSON form.
        assert!(
            vmess(&format!(
                r#"{{"add":"h","port":1,"id":"{UUID}","tls":"reality"}}"#
            ))
            .is_err()
        );
    }

    #[test]
    fn vmess_json_modes_and_insecure_round_trip() {
        let s = vmess(&format!(
            r#"{{"add":"g.example","port":443,"id":"{UUID}","net":"grpc","type":"multi","path":"svc","tls":"tls","insecure":"1"}}"#
        ))
        .unwrap();
        assert_eq!(
            s.transport,
            Transport::Grpc {
                service_name: Some("svc".into()),
                mode: Some("multi".into())
            }
        );
        assert!(matches!(
            s.security,
            Security::Tls {
                allow_insecure: true,
                ..
            }
        ));
        let link = to_link(&s);
        assert!(link.starts_with("vmess://ey"), "json form expected: {link}");
        assert_eq!(parse_link(&link).unwrap(), s);

        let x = vmess(&format!(
            r#"{{"add":"x.example","port":443,"id":"{UUID}","net":"xhttp","type":"packet-up","path":"/x"}}"#
        ))
        .unwrap();
        assert!(
            matches!(&x.transport, Transport::Xhttp { mode, .. } if mode.as_deref() == Some("packet-up"))
        );
        assert_eq!(parse_link(&to_link(&x)).unwrap(), x);
    }

    #[test]
    fn vmess_reality_exports_url_form() {
        let link = format!(
            "vmess://{UUID}@v.example:443?encryption=aes-128-gcm&aid=8&security=reality&pbk=PBK&sid=ab&spx=%2Fs&type=xhttp&mode=auto&extra=%7B%22a%22%3A1%7D#VR"
        );
        let s = parse_link(&link).unwrap();
        assert!(
            matches!(s.auth, Auth::Vmess { alter_id: 8, ref security, .. } if security == "aes-128-gcm")
        );
        let out = to_link(&s);
        assert!(out.starts_with(&format!("vmess://{UUID}@")), "{out}");
        assert_eq!(parse_link(&out).unwrap(), s);
    }

    #[test]
    fn xhttp_extra_is_parsed_canonically_and_validated() {
        let s = parse_link(&format!(
            "vless://{UUID}@h.example:443?type=xhttp&extra=%7B%20%22xPaddingBytes%22%20%3A%20%22100-1000%22%20%7D"
        ))
        .unwrap();
        assert!(matches!(&s.transport, Transport::Xhttp { extra, .. }
            if extra.as_deref() == Some(r#"{"xPaddingBytes":"100-1000"}"#)));
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
        for bad in ["%5B1%5D", "%7B", "null", "1", "%22x%22"] {
            let err = parse_link(&format!(
                "vless://{UUID}@h.example:443?type=xhttp&extra={bad}"
            ))
            .unwrap_err();
            assert!(
                matches!(&err, Error::MalformedLink(m) if m.contains("extra")),
                "{bad}"
            );
        }
        // A broken `extra` in a hand-edited profile fails validation.
        let mut s = s;
        if let Transport::Xhttp { extra, .. } = &mut s.transport {
            *extra = Some("[]".into());
        }
        assert!(s.validate().is_err());
    }

    #[test]
    fn reality_pqv_and_tls_pinning_round_trip() {
        let s = parse_link(&format!(
            "vless://{UUID}@h.example:443?security=reality&pbk=PBK&pqv=PQKEY&sni=a.example"
        ))
        .unwrap();
        assert!(
            matches!(&s.security, Security::Reality { mldsa65_verify, .. }
            if mldsa65_verify.as_deref() == Some("PQKEY"))
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
        let t = parse_link("trojan://pw@h.example:443?pcs=AB12,CD34&vcn=real.example").unwrap();
        assert!(
            matches!(&t.security, Security::Tls { pinned_peer_cert_sha256, verify_peer_cert_by_name, .. }
            if pinned_peer_cert_sha256.as_deref() == Some("AB12,CD34")
                && verify_peer_cert_by_name.as_deref() == Some("real.example"))
        );
        assert_eq!(parse_link(&to_link(&t)).unwrap(), t);
    }

    #[test]
    fn shadowsocks_plugin_is_rejected() {
        let userinfo = URL_SAFE_NO_PAD.encode("aes-256-gcm:secret");
        let err = parse_link(&format!(
            "ss://{userinfo}@h.example:8388?plugin=obfs-local%3Bobfs%3Dhttp%3Bobfs-host%3Dx#S"
        ))
        .unwrap_err();
        assert!(
            matches!(&err, Error::MalformedLink(m) if m.contains("plugin") && m.contains("obfs-local") && !m.contains("obfs-host")),
            "{err:?}"
        );
        // An empty `plugin=` is no plugin.
        assert!(parse_link(&format!("ss://{userinfo}@h.example:8388?plugin=#S")).is_ok());
    }

    #[test]
    fn legacy_shadowsocks_without_at_does_not_recurse() {
        for link in ["ss://#x", "ss://", "ss://Zm9v#x", "ss://    #y"] {
            assert!(parse_link(link).is_err(), "{link}");
        }
        // base64 of base64 is not unwrapped twice.
        let twice = STANDARD.encode(STANDARD.encode("aes-256-gcm:pw@h.example:1"));
        assert!(parse_link(&format!("ss://{twice}")).is_err());
    }

    #[test]
    fn socks_user_that_looks_like_base64_stays_literal() {
        let s = parse_link("socks://abcd@s.example:1080#S").unwrap();
        assert_eq!(
            s.auth,
            Auth::UserPass {
                user: Some("abcd".into()),
                pass: None
            }
        );
        assert_eq!(parse_link(&to_link(&s)).unwrap(), s);
    }

    #[test]
    fn uuid_whitespace_is_trimmed() {
        let s = parse_link(&format!("vless://%20{UUID}%20@h.example:443")).unwrap();
        assert!(matches!(&s.auth, Auth::Vless { uuid, .. } if uuid == UUID));
        assert!(parse_link("vless://%20%20@h.example:443").is_err());
    }

    #[test]
    fn lenient_base64_ignores_line_breaks() {
        let wrapped = "aGVs\nbG8g\r\nd29y bGQ=";
        assert_eq!(decode_base64_lenient(wrapped).unwrap(), b"hello world");
        assert_eq!(decode_base64_lenient("aGVsbG8").unwrap(), b"hello");
        assert_eq!(decode_base64_lenient("-_8").unwrap(), vec![0xfb, 0xff]);
        assert!(decode_base64_lenient("@@@").is_none());
    }
}
