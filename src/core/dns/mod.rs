use std::net::{IpAddr, Ipv4Addr};

/// Returns `true` if `addr` is a private, loopback, or reserved address.
///
/// Covers IPv4 private ranges (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16),
/// IPv4 loopback (127.0.0.0/8), IPv6 loopback (::1), IPv4-mapped IPv6
/// loopback (::ffff:127.0.0.0/104), and decimal integer representations of
/// any of the above (e.g. `"2130706433"` for 127.0.0.1).
pub fn is_private_ip(addr: &str) -> bool {
    let addr = addr.trim();

    if let Ok(ip) = addr.parse::<IpAddr>() {
        return is_private_ip_addr(ip);
    }

    if let Ok(n) = addr.parse::<u32>() {
        let octets = n.to_be_bytes();
        return is_private_ip_addr(IpAddr::V4(Ipv4Addr::new(
            octets[0], octets[1], octets[2], octets[3],
        )));
    }

    false
}

fn is_private_ip_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 10
                || o[0] == 127
                || (o[0] == 172 && (o[1] & 0xF0) == 0x10)
                || (o[0] == 192 && o[1] == 168)
        }
        IpAddr::V6(v6) => {
            let o = v6.octets();
            o == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
                || (o[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff] && o[12] == 127)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_ip_detects_ipv4_private_ranges() {
        assert!(is_private_ip("10.0.0.0"));
        assert!(is_private_ip("10.255.255.255"));
        assert!(is_private_ip("172.16.0.0"));
        assert!(is_private_ip("172.31.255.255"));
        assert!(!is_private_ip("172.32.0.1"));
        assert!(is_private_ip("192.168.0.0"));
        assert!(is_private_ip("192.168.255.255"));
        assert!(!is_private_ip("192.169.0.1"));
    }

    #[test]
    fn is_private_ip_detects_loopback() {
        assert!(is_private_ip("127.0.0.1"));
        assert!(is_private_ip("127.255.255.255"));
        assert!(is_private_ip("::1"));
        assert!(is_private_ip("::ffff:127.99.99.99"));
    }

    #[test]
    fn is_private_ip_detects_decimal_representations() {
        assert!(is_private_ip("2130706433"));
        assert!(is_private_ip("167772161"));
        assert!(is_private_ip("3232235521"));
    }

    #[test]
    fn is_private_ip_rejects_public_addresses() {
        assert!(!is_private_ip("8.8.8.8"));
        assert!(!is_private_ip("1.1.1.1"));
        assert!(!is_private_ip("2001:4860:4860::8888"));
        assert!(!is_private_ip(""));
    }

    #[test]
    fn is_private_ip_trims_whitespace() {
        assert!(is_private_ip("  127.0.0.1  "));
        assert!(is_private_ip("  ::1  "));
        assert!(!is_private_ip("  8.8.8.8  "));
    }
}
