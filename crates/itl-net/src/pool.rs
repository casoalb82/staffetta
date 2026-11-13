//! Source-address pool for puppets.
//!
//! Many networks limit connections per IP address. With a routed IPv6 prefix
//! (a /64 is typical) every puppet can connect from its own address, derived
//! deterministically from the person it represents so the same person always
//! comes from the same address.
//!
//! The operating system must allow binding to any address of the prefix, e.g.
//! on Linux: `ip -6 route add local 2001:db8:1:2::/64 dev lo` and
//! `sysctl net.ipv6.ip_nonlocal_bind=1`. See docs/INSTALL.md.

use std::net::Ipv6Addr;

/// splitmix64: tiny, well mixed, deterministic.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// An address inside `prefix/len`, chosen from `seed`.
pub fn pool_addr(prefix: Ipv6Addr, len: u8, seed: u64) -> Ipv6Addr {
    let len = len.min(128) as u32;
    let host_bits = 128 - len;
    if host_bits == 0 {
        return prefix;
    }
    let net_mask: u128 = if len == 0 { 0 } else { u128::MAX << host_bits };
    let host_mask: u128 = !net_mask;
    let r = ((mix(seed) as u128) << 64) | mix(seed ^ 0xa5a5_a5a5_5a5a_5a5a) as u128;
    let mut host = r & host_mask;
    if host == 0 {
        host = 1; // avoid the subnet-router anycast address
    }
    Ipv6Addr::from((u128::from(prefix) & net_mask) | host)
}

/// Parse `"2001:db8:1:2::/64"`.
pub fn parse_prefix(s: &str) -> Result<(Ipv6Addr, u8), String> {
    let (a, l) = s.split_once('/').ok_or("expected <ipv6>/<length>")?;
    let addr: Ipv6Addr = a
        .trim()
        .parse()
        .map_err(|_| "not an IPv6 address".to_string())?;
    let len: u8 = l
        .trim()
        .parse()
        .map_err(|_| "bad prefix length".to_string())?;
    if !(48..=128).contains(&len) {
        return Err("prefix length must be between 48 and 128".into());
    }
    Ok((addr, len))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(a: Ipv6Addr, p: Ipv6Addr, len: u8) -> bool {
        let m = u128::MAX << (128 - len as u32);
        u128::from(a) & m == u128::from(p) & m
    }

    #[test]
    fn stays_inside_the_prefix_and_is_deterministic() {
        let p: Ipv6Addr = "2001:db8:1:2::".parse().unwrap();
        for seed in 0..200u64 {
            let a = pool_addr(p, 64, seed);
            assert!(inside(a, p, 64), "{a}");
            assert_eq!(a, pool_addr(p, 64, seed));
        }
    }

    #[test]
    fn different_people_get_different_addresses() {
        let p: Ipv6Addr = "2001:db8:1:2::".parse().unwrap();
        let set: std::collections::HashSet<_> = (0..500u64).map(|s| pool_addr(p, 64, s)).collect();
        assert_eq!(set.len(), 500);
    }

    #[test]
    fn small_prefixes_and_edges() {
        let p: Ipv6Addr = "fd00::1234".parse().unwrap();
        assert_eq!(pool_addr(p, 128, 7), p);
        let q: Ipv6Addr = "fd00::".parse().unwrap();
        for s in 0..50u64 {
            assert!(inside(pool_addr(q, 120, s), q, 120));
        }
        assert_ne!(pool_addr(q, 127, 0), q, "never the all-zero host");
    }

    #[test]
    fn prefix_parsing() {
        assert_eq!(parse_prefix("2001:db8::/64").unwrap().1, 64);
        assert!(parse_prefix("2001:db8::").is_err());
        assert!(parse_prefix("10.0.0.0/8").is_err());
        assert!(parse_prefix("2001:db8::/16").is_err());
    }
}
