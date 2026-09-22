//! Offline IP → country lookup using the `geoip.dat` that ships with Xray
//! (v2fly format: a protobuf `GeoIPList`). No network, no third party.

use std::net::IpAddr;
use std::path::Path;

use prost::Message;

use crate::error::{Error, Result};

#[derive(Clone, PartialEq, Message)]
pub struct Cidr {
    #[prost(bytes = "vec", tag = "1")]
    pub ip: Vec<u8>,
    #[prost(uint32, tag = "2")]
    pub prefix: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct GeoIp {
    #[prost(string, tag = "1")]
    pub country_code: String,
    #[prost(message, repeated, tag = "2")]
    pub cidr: Vec<Cidr>,
    #[prost(bool, tag = "3")]
    pub reverse_match: bool,
}

#[derive(Clone, PartialEq, Message)]
pub struct GeoIpList {
    #[prost(message, repeated, tag = "1")]
    pub entry: Vec<GeoIp>,
}

/// A network range as (address as u128, prefix length, is_v6).
#[derive(Debug, Clone, Copy)]
struct Range {
    addr: u128,
    prefix: u8,
    v6: bool,
}

/// In-memory country database. Loading takes ~100 ms for the 17 MB file;
/// keep one instance around.
#[derive(Debug, Default)]
pub struct GeoIpDb {
    /// (country code, ranges) — codes are ISO 3166-1 alpha-2, uppercase.
    countries: Vec<(String, Vec<Range>)>,
}

impl GeoIpDb {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let list =
            GeoIpList::decode(bytes).map_err(|e| Error::Subscription(format!("geoip.dat: {e}")))?;
        let mut countries = Vec::with_capacity(list.entry.len());
        for entry in list.entry {
            let code = entry.country_code.to_ascii_uppercase();
            // Skip pseudo-countries; only real ISO codes get a flag.
            if code.len() != 2 || entry.reverse_match {
                continue;
            }
            let ranges = entry
                .cidr
                .iter()
                .filter_map(|c| match c.ip.len() {
                    4 => Some(Range {
                        addr: u128::from(u32::from_be_bytes([c.ip[0], c.ip[1], c.ip[2], c.ip[3]])),
                        prefix: c.prefix as u8,
                        v6: false,
                    }),
                    16 => {
                        let mut b = [0u8; 16];
                        b.copy_from_slice(&c.ip);
                        Some(Range {
                            addr: u128::from_be_bytes(b),
                            prefix: c.prefix as u8,
                            v6: true,
                        })
                    }
                    _ => None,
                })
                .collect();
            countries.push((code, ranges));
        }
        Ok(Self { countries })
    }

    pub fn is_empty(&self) -> bool {
        self.countries.is_empty()
    }

    /// Country code for `ip`, if any range contains it.
    pub fn lookup(&self, ip: IpAddr) -> Option<&str> {
        let (addr, v6) = match ip {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), false),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => (u128::from(u32::from(v4)), false),
                None => (u128::from(v6), true),
            },
        };
        for (code, ranges) in &self.countries {
            if ranges.iter().any(|r| r.v6 == v6 && contains(r, addr)) {
                return Some(code);
            }
        }
        None
    }
}

fn contains(r: &Range, addr: u128) -> bool {
    let bits: u32 = if r.v6 { 128 } else { 32 };
    if r.prefix == 0 {
        return true;
    }
    if u32::from(r.prefix) >= bits {
        return r.addr == addr;
    }
    let shift = bits - u32::from(r.prefix);
    (r.addr >> shift) == (addr >> shift)
}

/// Regional-indicator emoji flag for an ISO alpha-2 code (`"BR"` → 🇧🇷).
pub fn flag_emoji(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_uppercase();
    if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    Some(
        code.bytes()
            .map(|b| char::from_u32(0x1F1E6 + u32::from(b - b'A')).unwrap_or('?'))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> GeoIpDb {
        let list = GeoIpList {
            entry: vec![
                GeoIp {
                    country_code: "br".into(),
                    cidr: vec![
                        Cidr {
                            ip: vec![177, 0, 0, 0],
                            prefix: 8,
                        },
                        Cidr {
                            ip: vec![0x28, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                            prefix: 16,
                        },
                    ],
                    reverse_match: false,
                },
                GeoIp {
                    country_code: "US".into(),
                    cidr: vec![Cidr {
                        ip: vec![8, 8, 8, 0],
                        prefix: 24,
                    }],
                    reverse_match: false,
                },
                GeoIp {
                    country_code: "private".into(),
                    cidr: vec![Cidr {
                        ip: vec![10, 0, 0, 0],
                        prefix: 8,
                    }],
                    reverse_match: false,
                },
            ],
        };
        GeoIpDb::from_bytes(&list.encode_to_vec()).unwrap()
    }

    #[test]
    fn lookup_v4_v6_and_misses() {
        let db = db();
        assert_eq!(db.lookup("177.45.1.2".parse().unwrap()), Some("BR"));
        assert_eq!(db.lookup("8.8.8.8".parse().unwrap()), Some("US"));
        assert_eq!(db.lookup("8.8.9.1".parse().unwrap()), None);
        assert_eq!(db.lookup("2804:14d::1".parse().unwrap()), Some("BR"));
        assert_eq!(db.lookup("::ffff:8.8.8.8".parse().unwrap()), Some("US"));
        // Pseudo-country entries are ignored.
        assert_eq!(db.lookup("10.1.2.3".parse().unwrap()), None);
    }

    #[test]
    fn flags() {
        assert_eq!(flag_emoji("br").unwrap(), "\u{1F1E7}\u{1F1F7}");
        assert_eq!(flag_emoji("US").unwrap(), "🇺🇸");
        assert_eq!(flag_emoji("PRIVATE"), None);
        assert_eq!(flag_emoji("b1"), None);
    }

    #[test]
    fn real_geoip_if_available() {
        let path = crate::paths::core_dir().join("geoip.dat");
        if !path.exists() {
            eprintln!("skipping: no geoip.dat installed");
            return;
        }
        let db = GeoIpDb::load(&path).unwrap();
        assert!(!db.is_empty());
        assert_eq!(db.lookup("8.8.8.8".parse().unwrap()), Some("US"));
        assert_eq!(db.lookup("200.160.2.3".parse().unwrap()), Some("BR")); // registro.br
    }
}
