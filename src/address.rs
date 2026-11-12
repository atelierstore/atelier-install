//! This iPhone's Wi-Fi address, which the announcement has to carry and the
//! program can't find out: rootshell tells the programs it runs nothing
//! about the iPhone's network. rootshell's own `ifconfig en0` prints it, so
//! the address comes from that command's output, or as the user types it.

use std::net::Ipv4Addr;

/// The IPv4 address in `text`: `ifconfig`'s output for one interface, or an
/// address by itself. None when there's none to announce.
pub fn find(text: &str) -> Option<Ipv4Addr> {
    // ifconfig's own line, on a Mac or an iPhone ("inet 192.168.1.20 netmask
    // 0xffffff00 …") and on Linux ("inet addr:192.168.1.20 …").
    let from_ifconfig = text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != "inet" {
            return None;
        }
        usable(words.next()?.trim_start_matches("addr:"))
    });
    from_ifconfig.or_else(|| {
        // Otherwise an address by itself, as typed or pasted.
        let mut words = text.split_whitespace();
        let only = words.next()?;
        words.next().is_none().then(|| usable(only)).flatten()
    })
}

/// An address other devices, and this one, reach the iPhone's Wi-Fi at.
fn usable(word: &str) -> Option<Ipv4Addr> {
    let address: Ipv4Addr = word.parse().ok()?;
    let reachable = !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast();
    reachable.then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ifconfig_on_an_iphone() {
        let text = "en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
            \toptions=6460<TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVERT_CSUM>\n\
            \tether 1a:2b:3c:4d:5e:6f\n\
            \tinet6 fe80::1c2d:3e4f:5a6b:7c8d%en0 prefixlen 64 secured scopeid 0xe\n\
            \tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n\
            \tnd6 options=201<PERFORMNUD,DAD>\n\
            \tmedia: autoselect\n\
            \tstatus: active\n";
        assert_eq!(find(text), Some(Ipv4Addr::new(192, 168, 1, 20)));
    }

    #[test]
    fn ifconfig_on_linux() {
        let text = "eth0  Link encap:Ethernet\n      inet addr:10.0.0.7  Bcast:10.0.0.255\n";
        assert_eq!(find(text), Some(Ipv4Addr::new(10, 0, 0, 7)));
    }

    #[test]
    fn an_address_by_itself() {
        assert_eq!(
            find(" 192.168.1.20\n"),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
    }

    #[test]
    fn nothing_to_announce() {
        // Wi-Fi off: the interface has no inet line.
        assert_eq!(
            find("en0: flags=8822<BROADCAST,SMART,SIMPLEX,MULTICAST> mtu 1500\n"),
            None
        );
        assert_eq!(find("\tinet 127.0.0.1 netmask 0xff000000\n"), None);
        assert_eq!(find("ifconfig: interface en0 does not exist"), None);
        assert_eq!(find("192.168.1"), None);
        assert_eq!(find("0.0.0.0"), None);
        assert_eq!(find(""), None);
    }

    #[test]
    fn the_wifi_address_not_the_broadcast_one() {
        let text = "\tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n";
        assert_eq!(find(text), Some(Ipv4Addr::new(192, 168, 1, 20)));
    }
}
