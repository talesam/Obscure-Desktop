//! Opt-in check of the indexed GeoIP lookup against the original linear
//! scan on a real `geoip.dat`:
//!
//! ```text
//! OBSCURE_TEST_GEOIP=~/.local/share/obscure/core/geoip.dat \
//!   cargo test --release -p obscure-core --test geoip_real -- --nocapture
//! ```

use std::net::IpAddr;
use std::time::Instant;

use obscure_core::geoip::{GeoIpDb, GeoIpList};
use prost::Message;

/// One CIDR as the original implementation saw it.
struct Range {
    addr: u128,
    prefix: u32,
    v6: bool,
}

/// The pre-index implementation: try each country in file order and scan
/// all of its ranges.
struct Linear {
    countries: Vec<(String, Vec<Range>)>,
}

impl Linear {
    fn new(list: &GeoIpList) -> Self {
        let countries = list
            .entry
            .iter()
            .filter_map(|e| {
                let code = e.country_code.to_ascii_uppercase();
                if code.len() != 2 || e.reverse_match {
                    return None;
                }
                let ranges = e
                    .cidr
                    .iter()
                    .filter_map(|c| match c.ip.len() {
                        4 => Some(Range {
                            addr: u128::from(u32::from_be_bytes([
                                c.ip[0], c.ip[1], c.ip[2], c.ip[3],
                            ])),
                            prefix: c.prefix,
                            v6: false,
                        }),
                        16 => {
                            let mut b = [0u8; 16];
                            b.copy_from_slice(&c.ip);
                            Some(Range {
                                addr: u128::from_be_bytes(b),
                                prefix: c.prefix,
                                v6: true,
                            })
                        }
                        _ => None,
                    })
                    .collect();
                Some((code, ranges))
            })
            .collect();
        Self { countries }
    }

    fn lookup(&self, ip: IpAddr) -> Option<&str> {
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
    if r.prefix >= bits {
        return r.addr == addr;
    }
    let shift = bits - r.prefix;
    (r.addr >> shift) == (addr >> shift)
}

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn next_u128(&mut self) -> u128 {
        (u128::from(self.next()) << 64) | u128::from(self.next())
    }
}

#[test]
fn indexed_lookup_matches_linear_scan_on_real_file() {
    let Some(path) = std::env::var_os("OBSCURE_TEST_GEOIP") else {
        eprintln!("skipping: set OBSCURE_TEST_GEOIP=/path/to/geoip.dat");
        return;
    };
    let bytes = std::fs::read(&path).expect("read geoip.dat");

    let t = Instant::now();
    let db = GeoIpDb::from_bytes(&bytes).expect("indexed load");
    let load_new = t.elapsed();
    let t = Instant::now();
    let list = GeoIpList::decode(bytes.as_slice()).expect("decode");
    let linear = Linear::new(&list);
    let load_old = t.elapsed();

    // Probes: uniform IPv4, uniform global-unicast IPv6, and random hosts
    // inside random listed ranges (so most probes actually hit).
    let all: Vec<&obscure_core::geoip::Cidr> = list.entry.iter().flat_map(|e| &e.cidr).collect();
    let mut rng = XorShift(0x2545_F491_4F6C_DD1D);
    let mut probes: Vec<IpAddr> = Vec::new();
    for _ in 0..1500 {
        probes.push(IpAddr::from((rng.next() as u32).to_be_bytes()));
    }
    for _ in 0..500 {
        let a = (rng.next_u128() & (u128::MAX >> 3)) | (1u128 << 125);
        probes.push(IpAddr::from(a.to_be_bytes()));
    }
    for _ in 0..3000 {
        let c = all[(rng.next() % all.len() as u64) as usize];
        match c.ip.len() {
            4 => {
                let base = u32::from_be_bytes([c.ip[0], c.ip[1], c.ip[2], c.ip[3]]);
                let host = if c.prefix >= 32 {
                    0
                } else {
                    (rng.next() as u32) & (u32::MAX >> c.prefix)
                };
                probes.push(IpAddr::from((base | host).to_be_bytes()));
            }
            16 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(&c.ip);
                let base = u128::from_be_bytes(b);
                let host = if c.prefix >= 128 {
                    0
                } else {
                    rng.next_u128() & (u128::MAX >> c.prefix)
                };
                probes.push(IpAddr::from((base | host).to_be_bytes()));
            }
            _ => {}
        }
    }

    let t = Instant::now();
    let new: Vec<Option<String>> = probes
        .iter()
        .map(|ip| db.lookup(*ip).map(str::to_owned))
        .collect();
    let time_new = t.elapsed();
    let t = Instant::now();
    let old: Vec<Option<String>> = probes
        .iter()
        .map(|ip| linear.lookup(*ip).map(str::to_owned))
        .collect();
    let time_old = t.elapsed();

    let hits = new.iter().filter(|r| r.is_some()).count();
    eprintln!(
        "{} probes ({hits} hits); load: indexed {load_new:?} vs linear {load_old:?}; \
         lookups: indexed {time_new:?} vs linear {time_old:?}",
        probes.len()
    );
    for (i, ip) in probes.iter().enumerate() {
        assert_eq!(new[i], old[i], "mismatch for {ip}");
    }
    assert!(hits > probes.len() / 2, "most probes should hit");
}
