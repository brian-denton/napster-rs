//! IP addresses travel as the decimal value of the raw `in_addr` read on a
//! little-endian machine, so 1.2.3.4 becomes 0x04030201 = 67305985.

use std::net::Ipv4Addr;

pub fn encode(ip: Ipv4Addr) -> u32 {
    u32::from_le_bytes(ip.octets())
}

pub fn decode(n: u32) -> Ipv4Addr {
    Ipv4Addr::from(n.to_le_bytes())
}

/// Parse either the numeric form or a dotted quad.
pub fn parse(s: &str) -> Option<Ipv4Addr> {
    s.parse::<u32>().ok().map(decode).or_else(|| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn little_endian_in_addr() {
        assert_eq!(encode(Ipv4Addr::new(1, 2, 3, 4)), 0x04030201);
        assert_eq!(decode(3437166285), Ipv4Addr::new(205, 254, 222, 204));
        assert_eq!(parse("127.0.0.1"), Some(Ipv4Addr::LOCALHOST));
        assert_eq!(parse("16777343"), Some(Ipv4Addr::LOCALHOST));
    }
}
