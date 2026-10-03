//! Parse complete device IP addresses without guessing a local network prefix.
use std::net::IpAddr;

pub fn parse(input: &str) -> Result<IpAddr, String> {
    input
        .trim()
        .parse()
        .map_err(|_| "Enter an IPv4 or IPv6 address.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_complete_ipv4_and_ipv6_literals() {
        for input in ["192.168.1.42", "2001:db8::42", "::1"] {
            assert_eq!(
                parse(&format!("  {input}  ")).unwrap(),
                input.parse::<IpAddr>().unwrap()
            );
        }
    }
    #[test]
    fn rejects_short_codes_hostnames_urls_and_port_suffixes() {
        for input in [
            "",
            "#42",
            "42",
            "#0",
            "#255",
            "example.com",
            "https://192.168.1.2",
            "192.168.1.2:53317",
            "192.168.1.999",
        ] {
            assert!(parse(input).is_err(), "Unexpectedly accepted {input:?}");
        }
    }
}
