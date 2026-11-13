//! This iPhone's Wi-Fi address, which the announcement has to carry and the
//! program can't find out: rootshell tells the programs it runs nothing
//! about the iPhone's network. rootshell's own `ifconfig` prints it, so the
//! address comes from that command's output, or as the user types it.
//!
//! `ifconfig en0` prints the Wi-Fi alone. `ifconfig` prints every
//! interface, the mobile data's too, and LocalDevVPN's while it's
//! connected, which the page's Shortcut saves so the installer knows
//! whether to ask for it.

use std::net::Ipv4Addr;

/// The Wi-Fi's interface on an iPhone.
const WIFI: &str = "en0";
/// LocalDevVPN's end of its tunnel, where it takes the iPhone's services:
/// its interface lists it as the other end (`inet 10.7.1.1 --> 10.7.0.1`).
const LOCAL_DEV_VPN_PEER: &str = "10.7.0.1";

/// The IPv4 address in `text`: `ifconfig`'s output, for the Wi-Fi when it
/// lists several interfaces, or an address by itself. None when there's
/// none to announce.
pub fn find(text: &str) -> Option<Ipv4Addr> {
    let sections = interfaces(text);
    let text = if sections.is_empty() {
        text
    } else {
        sections
            .iter()
            .find(|(name, _)| *name == WIFI)
            .map(|(_, lines)| *lines)?
    };
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

/// Whether `text`, `ifconfig`'s output, lists LocalDevVPN's tunnel: it was
/// connected when the output was saved.
pub fn local_dev_vpn(text: &str) -> bool {
    interfaces(text).iter().any(|(name, lines)| {
        name.starts_with("utun")
            && lines.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some("inet") && words.any(|word| word == LOCAL_DEV_VPN_PEER)
            })
    })
}

/// `ifconfig`'s interfaces, each with its own lines, header included, as a
/// Mac or an iPhone prints them ("en0: flags=8863<…> mtu 1500", then lines
/// that start with a tab). None for another format.
fn interfaces(text: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut at = 0;
    let mut current: Option<(&str, usize)> = None;
    for line in text.split_inclusive('\n') {
        let header = (!line.starts_with(char::is_whitespace))
            .then(|| line.split_once(": flags="))
            .flatten()
            .map(|(name, _)| name)
            .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace));
        if let Some(name) = header {
            if let Some((previous, from)) = current.take() {
                found.push((previous, &text[from..at]));
            }
            current = Some((name, at));
        }
        at += line.len();
    }
    if let Some((name, from)) = current {
        found.push((name, &text[from..]));
    }
    found
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

    /// `ifconfig` on an iPhone, cut short: the mobile data's address comes
    /// before the Wi-Fi's, and LocalDevVPN's tunnel after it.
    const EVERY_INTERFACE: &str = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n\
        \tinet 127.0.0.1 netmask 0xff000000\n\
        pdp_ip0: flags=8011<UP,POINTOPOINT,MULTICAST> mtu 1500\n\
        \tinet 10.142.7.9 --> 10.142.7.9 netmask 0xff000000\n\
        en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n\
        \tether 1a:2b:3c:4d:5e:6f\n\
        \tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n\
        \tstatus: active\n\
        utun4: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1500\n\
        \tinet 10.7.1.1 --> 10.7.0.1 netmask 0xffffffff\n";

    #[test]
    fn the_wifi_among_every_interface() {
        assert_eq!(find(EVERY_INTERFACE), Some(Ipv4Addr::new(192, 168, 1, 20)));
        let without_wifi = EVERY_INTERFACE.replace("en0:", "en2:");
        assert_eq!(find(&without_wifi), None);
    }

    #[test]
    fn local_dev_vpn_when_connected() {
        assert!(local_dev_vpn(EVERY_INTERFACE));
        let cut = EVERY_INTERFACE.find("utun4:").unwrap();
        assert!(!local_dev_vpn(&EVERY_INTERFACE[..cut]));
        // Another VPN's tunnel isn't it.
        let other = EVERY_INTERFACE.replace("10.7.0.1", "10.9.0.1");
        assert!(!local_dev_vpn(&other));
        // `ifconfig en0` alone can't tell.
        assert!(!local_dev_vpn("\tinet 192.168.1.20 netmask 0xffffff00\n"));
    }

    #[test]
    fn the_wifi_address_not_the_broadcast_one() {
        let text = "\tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n";
        assert_eq!(find(text), Some(Ipv4Addr::new(192, 168, 1, 20)));
    }
}
