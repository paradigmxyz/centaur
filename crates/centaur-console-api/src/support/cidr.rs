//! CIDR blocks for request rules: `10.0.0.0/8`, `2001:db8::/32`, or a bare
//! address (a single-host block).

use std::{net::IpAddr, str::FromStr};

use ipnet::IpNet;

/// The block `value` names, or `None` when it is not an address or CIDR.
pub fn parse(value: &str) -> Option<IpNet> {
    let value = value.trim();
    IpNet::from_str(value)
        .ok()
        .or_else(|| IpAddr::from_str(value).ok().map(IpNet::from))
}

pub fn valid(value: &str) -> bool {
    parse(value).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_bare_addresses() {
        for ok in [
            "10.0.0.0/8",
            "192.168.1.1",
            "2001:db8::/32",
            "::1",
            " 172.16.0.0/12 ",
        ] {
            assert!(valid(ok), "{ok}");
        }
        for bad in [
            "",
            "10.0.0.0/33",
            "10.0.0/8",
            "example.test",
            "10.0.0.0/",
            "300.1.1.1",
            "::1/129",
        ] {
            assert!(!valid(bad), "{bad}");
        }
        assert_eq!(parse("10.1.2.3").unwrap().to_string(), "10.1.2.3/32");
        assert!(
            parse("10.0.0.0/8")
                .unwrap()
                .contains(&"10.9.9.9".parse::<IpAddr>().unwrap())
        );
    }
}
