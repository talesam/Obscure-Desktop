//! Subscription fetching: downloads a list of share links and reads the
//! de-facto standard headers (`subscription-userinfo`, `profile-title`,
//! `profile-update-interval`, `profile-web-page-url`, `support-url`).

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::links::{Server, parse_many};

/// Quota information from `subscription-userinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserInfo {
    pub upload: Option<u64>,
    pub download: Option<u64>,
    pub total: Option<u64>,
    /// Unix timestamp of expiry.
    pub expire: Option<u64>,
}

impl UserInfo {
    pub fn used(&self) -> Option<u64> {
        match (self.upload, self.download) {
            (None, None) => None,
            (u, d) => Some(u.unwrap_or(0) + d.unwrap_or(0)),
        }
    }

    /// Fraction used (0.0–1.0) when total is known and non-zero.
    pub fn fraction_used(&self) -> Option<f64> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.used()? as f64 / total as f64).min(1.0))
    }
}

/// Parses `upload=123; download=456; total=789; expire=1700000000`.
pub fn parse_userinfo(header: &str) -> UserInfo {
    let mut info = UserInfo::default();
    for part in header.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let v = v.trim().parse::<u64>().ok();
        match k.trim().to_ascii_lowercase().as_str() {
            "upload" => info.upload = v,
            "download" => info.download = v,
            "total" => info.total = v,
            "expire" => info.expire = v,
            _ => {}
        }
    }
    info
}

/// Result of fetching a subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    pub servers: Vec<Server>,
    /// Links that were present but could not be parsed.
    pub failed: Vec<String>,
    pub user_info: Option<UserInfo>,
    pub title: Option<String>,
    /// Suggested refresh interval.
    pub update_interval: Option<Duration>,
    pub web_page_url: Option<String>,
    pub support_url: Option<String>,
    /// Set when the server redirected permanently; callers should store it.
    pub new_url: Option<String>,
}

/// Decodes the subscription body: whole-body base64 (any alphabet, with or
/// without padding) or plain text.
pub fn decode_body(body: &str) -> Result<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(Error::Subscription("empty response".into()));
    }
    if looks_like_clash_yaml(trimmed) {
        return Err(Error::ClashYaml);
    }
    if trimmed.contains("://") {
        return Ok(trimmed.to_owned());
    }
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(&compact) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if looks_like_clash_yaml(&text) {
                return Err(Error::ClashYaml);
            }
            if text.contains("://") {
                return Ok(text);
            }
        }
    }
    Err(Error::Subscription(
        "body is neither share links nor base64".into(),
    ))
}

fn looks_like_clash_yaml(text: &str) -> bool {
    text.lines().take(50).any(|l| {
        l.starts_with("proxies:") || l.starts_with("proxy-groups:") || l.starts_with("mixed-port:")
    })
}

/// Decodes `profile-title`, which may be `base64:<...>`.
pub fn decode_title(header: &str) -> Option<String> {
    let h = header.trim();
    if h.is_empty() {
        return None;
    }
    if let Some(b64) = h.strip_prefix("base64:") {
        for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
            if let Ok(bytes) = engine.decode(b64.trim()) {
                return Some(String::from_utf8_lossy(&bytes).trim().to_owned());
            }
        }
        return None;
    }
    Some(h.to_owned())
}

/// Fetches and decodes a subscription.
pub async fn fetch(client: &reqwest::Client, url: &str) -> Result<Fetched> {
    let resp = client
        .get(url)
        .header("Accept", "text/plain, */*")
        .send()
        .await?
        .error_for_status()?;

    let headers = resp.headers().clone();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let final_url = resp.url().to_string();
    let new_url =
        header("moved-permanently-to").or_else(|| (final_url != url).then_some(final_url));

    let body = resp.text().await?;
    let text = decode_body(&body)?;
    let (servers, failed) = parse_many(&text);
    if servers.is_empty() {
        return Err(Error::Subscription("no servers in subscription".into()));
    }
    Ok(Fetched {
        servers,
        failed: failed.into_iter().map(|(l, _)| l).collect(),
        user_info: header("subscription-userinfo").map(|h| parse_userinfo(&h)),
        title: header("profile-title").and_then(|h| decode_title(&h)),
        update_interval: header("profile-update-interval")
            .and_then(|h| h.parse::<u64>().ok())
            .filter(|h| *h > 0)
            .map(|hours| Duration::from_secs(hours * 3600)),
        web_page_url: header("profile-web-page-url"),
        support_url: header("support-url"),
        new_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const UUID: &str = "2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c";

    #[test]
    fn userinfo_parsing_and_fraction() {
        let u = parse_userinfo("upload=100; download=300; total=1000; expire=1700000000");
        assert_eq!(u.upload, Some(100));
        assert_eq!(u.used(), Some(400));
        assert!((u.fraction_used().unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(u.expire, Some(1_700_000_000));
        let empty = parse_userinfo("garbage");
        assert_eq!(empty.used(), None);
        assert_eq!(empty.fraction_used(), None);
    }

    #[test]
    fn body_decoding() {
        let links = format!(
            "vless://{UUID}@a.example:443?security=none#A\nvless://{UUID}@b.example:443?security=none#B\n"
        );
        assert_eq!(decode_body(&links).unwrap(), links.trim());
        let b64 = STANDARD.encode(&links);
        // Line-wrapped base64 must also work.
        let wrapped: String = b64
            .as_bytes()
            .chunks(20)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(decode_body(&wrapped).unwrap(), links);
        assert_eq!(decode_body(&URL_SAFE_NO_PAD.encode(&links)).unwrap(), links);
        assert!(matches!(
            decode_body("proxies:\n  - name: x\n"),
            Err(Error::ClashYaml)
        ));
        assert!(matches!(
            decode_body(&STANDARD.encode("proxies:\n  - name: x\n")),
            Err(Error::ClashYaml)
        ));
        assert!(matches!(decode_body(""), Err(Error::Subscription(_))));
        assert!(matches!(
            decode_body("hello world"),
            Err(Error::Subscription(_))
        ));
    }

    #[test]
    fn title_decoding() {
        assert_eq!(decode_title("My Sub").as_deref(), Some("My Sub"));
        assert_eq!(
            decode_title(&format!("base64:{}", STANDARD.encode("Olá"))).as_deref(),
            Some("Olá")
        );
        assert_eq!(decode_title(""), None);
    }

    /// Minimal one-shot HTTP server returning `response` verbatim.
    async fn serve_once(response: String) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        });
        port
    }

    #[tokio::test]
    async fn fetch_reads_headers_and_links() {
        crate::init_crypto();
        let links = format!(
            "vless://{UUID}@a.example:443?security=none#A\ntrojan://pw@b.example:443#B\nvless://broken\n"
        );
        let body = STANDARD.encode(&links);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nsubscription-userinfo: upload=1; download=2; total=10; expire=99\r\nprofile-title: base64:{}\r\nprofile-update-interval: 12\r\nprofile-web-page-url: https://panel.example\r\nsupport-url: https://t.me/x\r\nConnection: close\r\n\r\n{}",
            body.len(),
            STANDARD.encode("Painel"),
            body
        );
        let port = serve_once(resp).await;
        let client = reqwest::Client::builder()
            .user_agent("Obscure/test")
            .build()
            .unwrap();
        let f = fetch(&client, &format!("http://127.0.0.1:{port}/sub"))
            .await
            .unwrap();
        assert_eq!(f.servers.len(), 2);
        assert_eq!(f.failed, vec!["vless://broken".to_owned()]);
        assert_eq!(f.title.as_deref(), Some("Painel"));
        assert_eq!(f.user_info.unwrap().total, Some(10));
        assert_eq!(f.update_interval, Some(Duration::from_secs(12 * 3600)));
        assert_eq!(f.web_page_url.as_deref(), Some("https://panel.example"));
        assert_eq!(f.support_url.as_deref(), Some("https://t.me/x"));
        assert_eq!(f.new_url, None);
    }

    #[tokio::test]
    async fn fetch_reports_clash_yaml() {
        crate::init_crypto();
        let body = "proxies:\n  - name: x\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let port = serve_once(resp).await;
        let client = reqwest::Client::new();
        let err = fetch(&client, &format!("http://127.0.0.1:{port}/"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ClashYaml));
    }
}
