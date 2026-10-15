//! IPv4 helpers shared by the watcher, ETW capture and profiler.

use std::net::Ipv4Addr;

/// Public, globally-routable unicast IPv4 (mirrors the server's filter).
pub fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224 // multicast + reserved + broadcast
        || (a == 100 && (64..=127).contains(&b)) // CGNAT
        || (a == 169 && b == 254) // link-local / cloud metadata
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

/// Parse "a.b.c.d/len" (len 0..=32) -> (network address, len). The address is masked.
pub fn parse_cidr(s: &str) -> Option<(Ipv4Addr, u8)> {
    let (ip, len) = s.split_once('/')?;
    let ip: Ipv4Addr = ip.trim().parse().ok()?;
    let len: u8 = len.trim().parse().ok()?;
    if len > 32 {
        return None;
    }
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    Some((Ipv4Addr::from(u32::from(ip) & mask), len))
}

#[allow(dead_code)]
pub fn cidr_contains(cidr: &str, ip: Ipv4Addr) -> bool {
    match parse_cidr(cidr) {
        Some((net, len)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(ip) & mask == u32::from(net)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn public_filter() {
        assert!(is_public_ipv4(ip("8.8.8.8")));
        assert!(is_public_ipv4(ip("172.217.1.1"))); // public 172.x
        assert!(is_public_ipv4(ip("172.64.0.1")));
        assert!(!is_public_ipv4(ip("172.16.0.1")));
        assert!(!is_public_ipv4(ip("172.31.255.255")));
        assert!(!is_public_ipv4(ip("10.1.2.3")));
        assert!(!is_public_ipv4(ip("192.168.1.1")));
        assert!(!is_public_ipv4(ip("169.254.169.254")));
        assert!(!is_public_ipv4(ip("100.64.0.1")));
        assert!(!is_public_ipv4(ip("224.0.0.251")));
        assert!(!is_public_ipv4(ip("255.255.255.255")));
        assert!(!is_public_ipv4(ip("127.0.0.1")));
    }

    #[test]
    fn cidr() {
        assert_eq!(parse_cidr("1.2.3.4/24"), Some((ip("1.2.3.0"), 24)));
        assert_eq!(parse_cidr("1.2.3.4/33"), None);
        assert!(cidr_contains("52.0.0.0/11", ip("52.20.1.1")));
        assert!(!cidr_contains("52.0.0.0/11", ip("52.40.1.1")));
        assert!(cidr_contains("1.2.3.4/32", ip("1.2.3.4")));
        assert!(!cidr_contains("garbage", ip("1.2.3.4")));
    }
}
