//! Latency tests: a quick TCP handshake to the server, and a real request
//! through a temporary Xray instance.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::{ConfigOptions, build};
use crate::error::{Error, Result};
use crate::links::Server;
use crate::supervisor::{Supervisor, write_config};

/// Colour buckets used by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatencyClass {
    Good,
    Fair,
    Poor,
}

pub fn classify(ms: u32) -> LatencyClass {
    match ms {
        0..150 => LatencyClass::Good,
        150..400 => LatencyClass::Fair,
        _ => LatencyClass::Poor,
    }
}

/// Time to complete a TCP handshake with `host:port`, in milliseconds.
pub async fn tcp_ping(host: &str, port: u16, timeout: Duration) -> Result<u32> {
    let start = Instant::now();
    let addr = format!("{host}:{port}");
    let connect = tokio::net::TcpStream::connect(&addr);
    match tokio::time::timeout(timeout, connect).await {
        Ok(Ok(_stream)) => Ok(start.elapsed().as_millis().min(u32::MAX as u128) as u32),
        Ok(Err(e)) => Err(Error::Latency(format!("{addr}: {e}"))),
        Err(_) => Err(Error::Latency(format!("{addr}: timed out"))),
    }
}

/// Picks a free TCP port on localhost.
pub async fn free_port() -> Result<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::Latency(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::Latency(e.to_string()))?
        .port();
    Ok(port)
}

/// Starts a temporary Xray for `server` on a random port and measures a
/// `HEAD` request to `url` through it. Returns the round trip in ms.
pub async fn url_test(
    supervisor: &Supervisor,
    server: &Server,
    scratch_dir: &Path,
    url: &str,
    timeout: Duration,
) -> Result<u32> {
    crate::init_crypto();
    let port = free_port().await?;
    let mut opts = ConfigOptions::new(server);
    opts.local_port = port;
    opts.log_level = "error";
    let cfg = build(&opts);
    let cfg_path = scratch_dir.join(format!("latency-{port}.json"));
    write_config(&cfg_path, &cfg)?;

    let core = supervisor.start(&cfg_path, port).await?;
    let result = measure(port, url, timeout).await;
    core.stop().await;
    let _ = std::fs::remove_file(&cfg_path);
    result
}

async fn measure(port: u16, url: &str, timeout: Duration) -> Result<u32> {
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}")).map_err(Error::Http)?)
        .timeout(timeout)
        .build()?;
    let start = Instant::now();
    let resp = client.head(url).send().await?;
    if resp.status().is_success() || resp.status().is_redirection() {
        Ok(start.elapsed().as_millis().min(u32::MAX as u128) as u32)
    } else {
        Err(Error::Latency(format!(
            "unexpected status {}",
            resp.status()
        )))
    }
}

/// Runs `tcp_ping` for many servers with bounded concurrency. Results are
/// returned in input order.
pub async fn tcp_ping_all(
    servers: &[Server],
    concurrency: usize,
    timeout: Duration,
) -> Vec<Result<u32>> {
    use futures_util::StreamExt;
    futures_util::stream::iter(
        servers
            .iter()
            .map(|s| async move { tcp_ping(&s.address, s.port, timeout).await }),
    )
    .buffered(concurrency.max(1))
    .collect()
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(classify(0), LatencyClass::Good);
        assert_eq!(classify(149), LatencyClass::Good);
        assert_eq!(classify(150), LatencyClass::Fair);
        assert_eq!(classify(400), LatencyClass::Poor);
    }

    #[tokio::test]
    async fn tcp_ping_local_listener_and_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ms = tcp_ping("127.0.0.1", port, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(ms < 1000);
        drop(listener);
        let err = tcp_ping("127.0.0.1", port, Duration::from_millis(500))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Latency(_)));
    }

    #[tokio::test]
    async fn ping_all_keeps_order() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let ok = crate::links::parse_link(&format!(
            "vless://2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c@127.0.0.1:{port}?security=none#ok"
        ))
        .unwrap();
        let bad = crate::links::parse_link(
            "vless://2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c@127.0.0.1:1?security=none#bad",
        )
        .unwrap();
        let r = tcp_ping_all(&[bad.clone(), ok, bad], 8, Duration::from_millis(500)).await;
        assert!(r[0].is_err());
        assert!(r[1].is_ok());
        assert!(r[2].is_err());
    }
}
