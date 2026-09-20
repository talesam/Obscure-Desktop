//! Traffic statistics from Xray's gRPC `StatsService`.
//!
//! The protobuf messages are tiny and stable, so they are written by hand
//! with `prost` derives instead of running `protoc` at build time (which
//! would complicate Flatpak and distro builds). Source of truth:
//! `app/stats/command/command.proto` in Xray-core.

use std::time::{Duration, Instant};

use prost::Message;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::Channel;
use tonic_prost::ProstCodec;

use crate::error::{Error, Result};

#[derive(Clone, PartialEq, Message)]
pub struct QueryStatsRequest {
    #[prost(string, tag = "1")]
    pub pattern: String,
    #[prost(bool, tag = "2")]
    pub reset: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct Stat {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(int64, tag = "2")]
    pub value: i64,
}

#[derive(Clone, PartialEq, Message)]
pub struct QueryStatsResponse {
    #[prost(message, repeated, tag = "1")]
    pub stat: Vec<Stat>,
}

const QUERY_STATS_PATH: &str = "/xray.app.stats.command.StatsService/QueryStats";

/// Cumulative bytes through the `proxy` outbound since Xray started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Traffic {
    pub uplink: u64,
    pub downlink: u64,
}

/// Snapshot with rates computed from the previous sample.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TrafficSample {
    pub total: Traffic,
    /// Bytes per second since the previous sample.
    pub up_rate: f64,
    pub down_rate: f64,
}

/// gRPC client for `StatsService` on `127.0.0.1:<port>`.
#[derive(Debug, Clone)]
pub struct StatsClient {
    grpc: tonic::client::Grpc<Channel>,
}

impl StatsClient {
    /// Connects lazily; the first query establishes the connection.
    pub fn new(port: u16) -> Result<Self> {
        let channel = Channel::from_shared(format!("http://127.0.0.1:{port}"))
            .map_err(|e| Error::Stats(e.to_string()))?
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .connect_lazy();
        Ok(Self {
            grpc: tonic::client::Grpc::new(channel),
        })
    }

    /// Runs `QueryStats` with the given pattern (empty = everything).
    pub async fn query(&mut self, pattern: &str) -> Result<Vec<Stat>> {
        self.grpc
            .ready()
            .await
            .map_err(|e| Error::Stats(e.to_string()))?;
        let codec: ProstCodec<QueryStatsRequest, QueryStatsResponse> = ProstCodec::default();
        let path = PathAndQuery::from_static(QUERY_STATS_PATH);
        let req = tonic::Request::new(QueryStatsRequest {
            pattern: pattern.to_owned(),
            reset: false,
        });
        let resp = self
            .grpc
            .unary(req, path, codec)
            .await
            .map_err(|e| Error::Stats(e.to_string()))?;
        Ok(resp.into_inner().stat)
    }

    /// Cumulative traffic of the `proxy` outbound.
    pub async fn proxy_traffic(&mut self) -> Result<Traffic> {
        let stats = self.query("outbound>>>proxy>>>traffic>>>").await?;
        Ok(traffic_from_stats(&stats))
    }
}

/// Extracts uplink/downlink of the `proxy` outbound from raw stats.
pub fn traffic_from_stats(stats: &[Stat]) -> Traffic {
    let mut t = Traffic::default();
    for s in stats {
        let v = u64::try_from(s.value).unwrap_or(0);
        match s.name.as_str() {
            "outbound>>>proxy>>>traffic>>>uplink" => t.uplink = v,
            "outbound>>>proxy>>>traffic>>>downlink" => t.downlink = v,
            _ => {}
        }
    }
    t
}

/// Turns successive `Traffic` totals into rates.
#[derive(Debug, Default)]
pub struct RateMeter {
    last: Option<(Instant, Traffic)>,
}

impl RateMeter {
    pub fn update(&mut self, total: Traffic) -> TrafficSample {
        self.update_at(Instant::now(), total)
    }

    fn update_at(&mut self, now: Instant, total: Traffic) -> TrafficSample {
        let (up_rate, down_rate) = match self.last {
            Some((t0, prev)) => {
                let secs = now.saturating_duration_since(t0).as_secs_f64().max(1e-3);
                (
                    total.uplink.saturating_sub(prev.uplink) as f64 / secs,
                    total.downlink.saturating_sub(prev.downlink) as f64 / secs,
                )
            }
            None => (0.0, 0.0),
        };
        self.last = Some((now, total));
        TrafficSample {
            total,
            up_rate,
            down_rate,
        }
    }
}

/// Formats bytes as `1.2 MB`, `340 kB`, etc.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Formats a rate as `1.2 MB/s`.
pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec.max(0.0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prost_roundtrip_matches_wire_format() {
        let req = QueryStatsRequest {
            pattern: "outbound>>>proxy".into(),
            reset: true,
        };
        let bytes = req.encode_to_vec();
        // field 1 (string) = 0x0a, field 2 (varint) = 0x10
        assert_eq!(bytes[0], 0x0a);
        assert!(bytes.ends_with(&[0x10, 0x01]));
        assert_eq!(QueryStatsRequest::decode(&bytes[..]).unwrap(), req);

        let resp = QueryStatsResponse {
            stat: vec![Stat {
                name: "outbound>>>proxy>>>traffic>>>uplink".into(),
                value: 12345,
            }],
        };
        let decoded = QueryStatsResponse::decode(&resp.encode_to_vec()[..]).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn traffic_and_rates() {
        let stats = vec![
            Stat {
                name: "outbound>>>proxy>>>traffic>>>uplink".into(),
                value: 1000,
            },
            Stat {
                name: "outbound>>>proxy>>>traffic>>>downlink".into(),
                value: 5000,
            },
            Stat {
                name: "outbound>>>direct>>>traffic>>>downlink".into(),
                value: 99,
            },
        ];
        let t = traffic_from_stats(&stats);
        assert_eq!(
            t,
            Traffic {
                uplink: 1000,
                downlink: 5000
            }
        );

        let mut meter = RateMeter::default();
        let t0 = Instant::now();
        let s0 = meter.update_at(t0, t);
        assert_eq!(s0.up_rate, 0.0);
        let s1 = meter.update_at(
            t0 + Duration::from_secs(2),
            Traffic {
                uplink: 3000,
                downlink: 5000,
            },
        );
        assert!((s1.up_rate - 1000.0).abs() < 1e-6);
        assert_eq!(s1.down_rate, 0.0);
    }

    #[test]
    fn formats() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1500), "1.5 kB");
        assert_eq!(format_bytes(123_456_789), "123 MB");
        assert_eq!(format_rate(2048.0), "2.0 kB/s");
    }

    #[tokio::test]
    async fn query_fails_cleanly_without_server() {
        let mut c = StatsClient::new(1).unwrap();
        assert!(matches!(c.query("").await, Err(Error::Stats(_))));
    }
}
