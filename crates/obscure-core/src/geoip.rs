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

/// A disjoint address range `[start, end]` that resolves to the country at
/// `country` (index into [`GeoIpDb::codes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span<T> {
    start: T,
    end: T,
    country: u16,
}

/// In-memory country database. Loading takes a few hundred ms for the
/// 17 MB file; keep one instance around. Lookups are a binary search.
#[derive(Debug, Default)]
pub struct GeoIpDb {
    /// Country codes, ISO 3166-1 alpha-2, uppercase, in file order.
    codes: Vec<String>,
    /// Sorted, non-overlapping IPv4 spans.
    v4: Vec<Span<u32>>,
    /// Sorted, non-overlapping IPv6 spans.
    v6: Vec<Span<u128>>,
}

impl GeoIpDb {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        Self::from_bytes(&bytes)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let list =
            GeoIpList::decode(bytes).map_err(|e| Error::Subscription(format!("geoip.dat: {e}")))?;
        let mut codes = Vec::with_capacity(list.entry.len());
        let mut v4: Vec<Vec<(u128, u128)>> = Vec::with_capacity(list.entry.len());
        let mut v6: Vec<Vec<(u128, u128)>> = Vec::with_capacity(list.entry.len());
        for entry in list.entry {
            let code = entry.country_code.to_ascii_uppercase();
            // Skip pseudo-countries; only real ISO codes get a flag.
            if code.len() != 2 || entry.reverse_match {
                continue;
            }
            if codes.len() > usize::from(u16::MAX) {
                break;
            }
            let mut r4 = Vec::new();
            let mut r6 = Vec::new();
            for c in &entry.cidr {
                match c.ip.len() {
                    4 => {
                        let addr = u32::from_be_bytes([c.ip[0], c.ip[1], c.ip[2], c.ip[3]]);
                        if let Some(r) = cidr_range(u128::from(addr), c.prefix, 32) {
                            r4.push(r);
                        }
                    }
                    16 => {
                        let mut b = [0u8; 16];
                        b.copy_from_slice(&c.ip);
                        if let Some(r) = cidr_range(u128::from_be_bytes(b), c.prefix, 128) {
                            r6.push(r);
                        }
                    }
                    _ => {}
                }
            }
            codes.push(code);
            v4.push(r4);
            v6.push(r6);
        }
        let v4 = flatten(v4, u128::from(u32::MAX))
            .into_iter()
            .map(|s| Span {
                // Every v4 range was built from a u32 and a mask: no loss.
                start: s.start as u32,
                end: s.end as u32,
                country: s.country,
            })
            .collect();
        let v6 = flatten(v6, u128::MAX);
        Ok(Self { codes, v4, v6 })
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Country code for `ip`, if any range contains it. When ranges of
    /// different countries overlap, the country listed first in the file
    /// wins (the order in which the original linear scan tried them).
    pub fn lookup(&self, ip: IpAddr) -> Option<&str> {
        let country = match ip {
            IpAddr::V4(v4) => find(&self.v4, u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => find(&self.v4, u32::from(v4)),
                None => find(&self.v6, u128::from(v6)),
            },
        }?;
        self.codes.get(usize::from(country)).map(String::as_str)
    }
}

fn find<T: Ord + Copy>(spans: &[Span<T>], addr: T) -> Option<u16> {
    let i = spans.partition_point(|s| s.start <= addr).checked_sub(1)?;
    let s = spans[i];
    (addr <= s.end).then_some(s.country)
}

/// `[first, last]` addresses of `addr/prefix` in a `bits`-wide family, or
/// `None` if the prefix is out of range. Host bits in `addr` are ignored.
fn cidr_range(addr: u128, prefix: u32, bits: u32) -> Option<(u128, u128)> {
    if prefix > bits {
        return None;
    }
    let host_bits = bits - prefix;
    let host_mask = if host_bits == 0 {
        0
    } else if host_bits >= 128 {
        u128::MAX
    } else {
        (1u128 << host_bits) - 1
    };
    let family_max = if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    };
    let start = addr & !host_mask & family_max;
    Some((start, start | host_mask))
}

/// Turns per-country range lists (index = priority, lower wins) into one
/// sorted list of disjoint spans, merging neighbours of the same country.
fn flatten(per_country: Vec<Vec<(u128, u128)>>, family_max: u128) -> Vec<Span<u128>> {
    use std::collections::BTreeMap;

    // Sweep events: (position, country, +1 start / -1 end-after).
    let mut events: Vec<(u128, u16, i8)> = Vec::new();
    for (idx, mut ranges) in per_country.into_iter().enumerate() {
        let idx = idx as u16;
        // Merge each country's own ranges first: fewer events.
        ranges.sort_unstable();
        let mut merged: Vec<(u128, u128)> = Vec::with_capacity(ranges.len());
        for (s, e) in ranges {
            match merged.last_mut() {
                Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        for (s, e) in merged {
            events.push((s, idx, 1));
            if e < family_max {
                events.push((e + 1, idx, -1));
            }
        }
    }
    events.sort_unstable_by_key(|&(pos, _, _)| pos);

    let mut active: BTreeMap<u16, u32> = BTreeMap::new();
    let mut spans: Vec<Span<u128>> = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let pos = events[i].0;
        while i < events.len() && events[i].0 == pos {
            let (_, idx, delta) = events[i];
            if delta > 0 {
                *active.entry(idx).or_insert(0) += 1;
            } else if let Some(n) = active.get_mut(&idx) {
                *n -= 1;
                if *n == 0 {
                    active.remove(&idx);
                }
            }
            i += 1;
        }
        let Some((&country, _)) = active.first_key_value() else {
            continue;
        };
        let end = match events.get(i) {
            Some(&(next, _, _)) => next - 1,
            None => family_max,
        };
        match spans.last_mut() {
            Some(last) if last.country == country && last.end.checked_add(1) == Some(pos) => {
                last.end = end;
            }
            _ => spans.push(Span {
                start: pos,
                end,
                country,
            }),
        }
    }
    spans
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

    fn entry(code: &str, cidrs: &[(&[u8], u32)]) -> GeoIp {
        GeoIp {
            country_code: code.into(),
            cidr: cidrs
                .iter()
                .map(|(ip, prefix)| Cidr {
                    ip: ip.to_vec(),
                    prefix: *prefix,
                })
                .collect(),
            reverse_match: false,
        }
    }

    fn build(entries: Vec<GeoIp>) -> GeoIpDb {
        GeoIpDb::from_bytes(&GeoIpList { entry: entries }.encode_to_vec()).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// The original linear scan, kept as the reference implementation.
    fn linear(list: &GeoIpList, ip: IpAddr) -> Option<String> {
        let (addr, v6) = match ip {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), false),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => (u128::from(u32::from(v4)), false),
                None => (u128::from(v6), true),
            },
        };
        let bits: u32 = if v6 { 128 } else { 32 };
        for e in &list.entry {
            let code = e.country_code.to_ascii_uppercase();
            if code.len() != 2 || e.reverse_match {
                continue;
            }
            let hit = e.cidr.iter().any(|c| {
                let base = match (c.ip.len(), v6) {
                    (4, false) => {
                        u128::from(u32::from_be_bytes([c.ip[0], c.ip[1], c.ip[2], c.ip[3]]))
                    }
                    (16, true) => {
                        let mut b = [0u8; 16];
                        b.copy_from_slice(&c.ip);
                        u128::from_be_bytes(b)
                    }
                    _ => return false,
                };
                if c.prefix > bits {
                    return false;
                }
                if c.prefix == 0 {
                    return true;
                }
                let shift = bits - c.prefix;
                if shift == 0 {
                    return base == addr;
                }
                (base >> shift) == (addr >> shift)
            });
            if hit {
                return Some(code);
            }
        }
        None
    }

    #[test]
    fn nested_ranges_first_listed_country_wins() {
        // A /8 listed before a /24 inside it: the /8's country wins, as in
        // the original scan (countries were tried in file order).
        let db = build(vec![
            entry("AA", &[(&[10, 0, 0, 0], 8)]),
            entry("BB", &[(&[10, 1, 2, 0], 24)]),
        ]);
        assert_eq!(db.lookup(ip("10.1.2.3")), Some("AA"));
        assert_eq!(db.lookup(ip("10.200.0.1")), Some("AA"));
        // Reversed order: the inner range punches a hole in the outer one.
        let db = build(vec![
            entry("BB", &[(&[10, 1, 2, 0], 24)]),
            entry("AA", &[(&[10, 0, 0, 0], 8)]),
        ]);
        assert_eq!(db.lookup(ip("10.1.1.255")), Some("AA"));
        assert_eq!(db.lookup(ip("10.1.2.0")), Some("BB"));
        assert_eq!(db.lookup(ip("10.1.2.255")), Some("BB"));
        assert_eq!(db.lookup(ip("10.1.3.0")), Some("AA"));
        assert_eq!(db.lookup(ip("10.255.255.255")), Some("AA"));
        assert_eq!(db.lookup(ip("11.0.0.0")), None);
        assert_eq!(db.lookup(ip("9.255.255.255")), None);
    }

    #[test]
    fn prefix_edge_cases() {
        let v6_net = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let db = build(vec![
            // Invalid prefixes are skipped (300 used to truncate to 44).
            entry("XX", &[(&[1, 2, 3, 4], 33), (&[1, 2, 3, 5], 300)]),
            // Host bits set in the address are ignored.
            entry("HB", &[(&[192, 168, 1, 77], 24)]),
            entry("ON", &[(&[5, 6, 7, 8], 32)]),
            entry("VS", &[(&[0xff; 16], 128), (&v6_net, 129)]),
            entry("ZZ", &[(&[0, 0, 0, 0], 0)]),
        ]);
        assert_eq!(db.lookup(ip("1.2.3.4")), Some("ZZ"));
        assert_eq!(db.lookup(ip("1.2.3.5")), Some("ZZ"));
        assert_eq!(db.lookup(ip("192.168.1.0")), Some("HB"));
        assert_eq!(db.lookup(ip("192.168.1.255")), Some("HB"));
        assert_eq!(db.lookup(ip("5.6.7.8")), Some("ON"));
        assert_eq!(db.lookup(ip("5.6.7.9")), Some("ZZ"));
        assert_eq!(db.lookup(ip("255.255.255.255")), Some("ZZ"));
        assert_eq!(db.lookup(ip("0.0.0.0")), Some("ZZ"));
        assert_eq!(
            db.lookup(ip("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")),
            Some("VS")
        );
        assert_eq!(db.lookup(ip("2001:db8::1")), None);
        assert_eq!(db.lookup(ip("::")), None);
    }

    #[test]
    fn empty_and_garbage() {
        assert!(GeoIpDb::default().lookup(ip("1.1.1.1")).is_none());
        assert!(build(vec![]).is_empty());
        let db = build(vec![entry("AA", &[(&[1, 2, 3], 8)])]);
        assert!(!db.is_empty());
        assert_eq!(db.lookup(ip("1.2.3.4")), None);
        assert!(GeoIpDb::from_bytes(&[0xff, 0xff, 0xff]).is_err());
    }

    /// Random overlapping databases: the binary search must agree with the
    /// linear reference on every probe.
    #[test]
    fn matches_linear_reference_on_random_data() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..40u8 {
            let mut entries = Vec::new();
            for c in 0..(1 + next() % 8) as u8 {
                let code: String = [(b'A' + c) as char, (b'A' + round % 26) as char]
                    .iter()
                    .collect();
                let mut cidrs = Vec::new();
                for _ in 0..(next() % 12) {
                    // Small address space so that ranges collide.
                    if next() % 3 == 0 {
                        let mut b = [0u8; 16];
                        b[0] = 0x20;
                        b[1] = (next() % 4) as u8;
                        b[2] = (next() % 4) as u8;
                        let prefix = (next() % 40) as u32;
                        cidrs.push(Cidr {
                            ip: b.to_vec(),
                            prefix,
                        });
                    } else {
                        let a = [10, (next() % 4) as u8, (next() % 4) as u8, next() as u8];
                        let prefix = (next() % 34) as u32;
                        cidrs.push(Cidr {
                            ip: a.to_vec(),
                            prefix,
                        });
                    }
                }
                entries.push(GeoIp {
                    country_code: code,
                    cidr: cidrs,
                    reverse_match: false,
                });
            }
            let list = GeoIpList { entry: entries };
            let db = GeoIpDb::from_bytes(&list.encode_to_vec()).unwrap();
            for _ in 0..500 {
                let probe = if next() % 3 == 0 {
                    let mut b = [0u8; 16];
                    b[0] = 0x20;
                    b[1] = (next() % 4) as u8;
                    b[2] = (next() % 4) as u8;
                    b[15] = next() as u8;
                    IpAddr::from(b)
                } else {
                    IpAddr::from([10, (next() % 4) as u8, (next() % 4) as u8, next() as u8])
                };
                assert_eq!(
                    db.lookup(probe).map(str::to_owned),
                    linear(&list, probe),
                    "round {round}, probe {probe}"
                );
            }
        }
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
