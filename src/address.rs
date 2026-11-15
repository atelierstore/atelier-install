//! This iPhone's Wi-Fi address, which the announcement has to carry and the
//! program can't find out: rootshell tells the programs it runs nothing
//! about the iPhone's network. rootshell's own `ifconfig` prints it, so the
//! address comes from that command's output, or as the user types it.
//!
//! `ifconfig en0` prints the Wi-Fi alone. `ifconfig` prints every
//! interface, the mobile data's too, and LocalDevVPN's while it's
//! connected, which the page's Shortcut saves so the installer knows
//! whether to ask for it.
//!
//! rootshell's `ifconfig` leaves out the line break after each
//! `hwassist=` line, so the line after it follows on the same line: an
//! interface's `inet`, after a tab, or the next interface's header
//! (`…TSO_V6>utun0: flags=8051<…>`). Seen on an iPhone on 14 Nov 2026.

use std::net::{IpAddr, Ipv4Addr};

/// The Wi-Fi's interface on an iPhone.
const WIFI: &str = "en0";
/// LocalDevVPN's interface's address, which it lists as both ends
/// (`inet 10.7.1.1 --> 10.7.1.1`, on an iPhone on 14 Nov 2026), and the
/// end where it takes the iPhone's services, which another build may list.
const LOCAL_DEV_VPN: [&str; 2] = ["10.7.1.1", "10.7.0.1"];

/// The IPv4 address in `text`: `ifconfig`'s output, for the Wi-Fi when it
/// lists several interfaces, or an address by itself. None when there's
/// none to announce.
pub fn find(text: &str) -> Option<Ipv4Addr> {
    let text = &unglued(text);
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

/// Every address `text`, `ifconfig`'s output, gives this iPhone, on every
/// interface and IPv6's too, but loopback and LocalDevVPN's: the ones
/// Bonjour can list for its pairing service.
pub fn all(text: &str) -> Vec<IpAddr> {
    let mut found: Vec<IpAddr> = Vec::new();
    for line in unglued(text).lines() {
        let mut words = line.split_whitespace();
        if !matches!(words.next(), Some("inet" | "inet6")) {
            continue;
        }
        let Some(word) = words.next().map(|word| word.trim_start_matches("addr:")) else {
            continue;
        };
        // An IPv6 address can end with its interface, %en0.
        let Some(Ok(address)) = word.split('%').next().map(str::parse::<IpAddr>) else {
            continue;
        };
        if !address.is_loopback()
            && !address.is_unspecified()
            && !LOCAL_DEV_VPN.contains(&word)
            && !found.contains(&address)
        {
            found.push(address);
        }
    }
    found
}

/// Whether `text`, `ifconfig`'s output, lists LocalDevVPN's tunnel: it was
/// connected when the output was saved.
pub fn local_dev_vpn(text: &str) -> bool {
    interfaces(&unglued(text)).iter().any(|(name, lines)| {
        name.starts_with("utun")
            && lines.lines().any(|line| {
                let mut words = line.split_whitespace();
                words.next() == Some("inet") && words.any(|word| LOCAL_DEV_VPN.contains(&word))
            })
    })
}

/// `text` with the line breaks rootshell's `ifconfig` leaves out put back:
/// before each tab that isn't at a line's start, and before each header
/// that follows something else on its line.
fn unglued(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    let mut previous = '\n';
    for (at, character) in text.char_indices() {
        let starts_line = previous == '\n';
        let glued_tab = character == '\t' && !starts_line;
        let glued_header = !starts_line
            && !name_character(previous)
            && !previous.is_whitespace()
            && header_at(&text[at..]);
        if glued_tab || glued_header {
            out.push('\n');
        }
        out.push(character);
        previous = character;
    }
    out
}

/// Whether `text` starts with an interface's header: its name, such as
/// en0 or pdp_ip0, then `: flags=`.
fn header_at(text: &str) -> bool {
    let name = text
        .find(|character| !name_character(character))
        .unwrap_or(text.len());
    name > 0 && text[name..].starts_with(": flags=")
}

fn name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
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
    fn every_address_of_this_iphone() {
        let text = format!(
            "{EVERY_INTERFACE}en1: flags=8863<UP> mtu 1500\n\
             \tinet6 fe80::1c2d:3e4f:5a6b:7c8d%en1 prefixlen 64 scopeid 0xf\n\
             \tinet6 2001:db8::20 prefixlen 64 autoconf secured\n"
        );
        let all: Vec<String> = all(&text).iter().map(ToString::to_string).collect();
        assert_eq!(
            all,
            [
                "10.142.7.9",
                "192.168.1.20",
                "fe80::1c2d:3e4f:5a6b:7c8d",
                "2001:db8::20"
            ]
        );
        assert!(super::all("ifconfig: interface en0 does not exist").is_empty());
    }

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
        let other = EVERY_INTERFACE.replace("10.7.1.1 --> 10.7.0.1", "10.9.1.1 --> 10.9.0.1");
        assert!(!local_dev_vpn(&other));
        // `ifconfig en0` alone can't tell.
        assert!(!local_dev_vpn("\tinet 192.168.1.20 netmask 0xffffff00\n"));
    }

    /// `ifconfig` as rootshell prints it on an iPhone, cut short, with its
    /// addresses changed: no line break after `hwassist=`, and LocalDevVPN
    /// with its own address at both ends.
    const ROOTSHELLS: &str = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384 index 1\n\
        \toptions=1203<RXCSUM,TXCSUM,TXSTATUS,SW_TIMESTAMP>\n\
        \thwassist=29000ff<CSUM_IP,CSUM_TCP,MULTIPAGES,TXSTATUS,SW_TIMESTAMP>\tinet 127.0.0.1 netmask 0xff000000\n\
        \tinet6 ::1 prefixlen 128 \n\
        pdp_ip0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1450 index 2\n\
        \thwassist=703000<CSUM_PARTIAL,MULTIPAGES,TSO_V4,TSO_V6>\tinet 10.142.7.9 --> 10.142.7.9 netmask 0xffffffff\n\
        ipsec2: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1500 index 23\n\
        \thwassist=703000<CSUM_PARTIAL,MULTIPAGES,TSO_V4,TSO_V6>utun0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1380 index 14\n\
        \tinet6 fe80::1%utun0 prefixlen 64 scopeid 0xe \n\
        en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500 index 16\n\
        \toptions=400<CHANNEL_IO>\n\
        \tinet6 fe80::2%en0 prefixlen 64 secured scopeid 0x10 \n\
        \tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n\
        utun5: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1500 index 39\n\
        \toptions=6460<TSO4,TSO6,CHANNEL_IO,PARTIAL_CSUM,ZEROINVERT_CSUM>\n\
        \thwassist=703000<CSUM_PARTIAL,MULTIPAGES,TSO_V4,TSO_V6>\tinet 10.7.1.1 --> 10.7.1.1 netmask 0xffffffff\n\
        \tnd6 options=201<PERFORMNUD,DAD>\n";

    #[test]
    fn rootshells_ifconfig_with_its_lines_run_together() {
        assert_eq!(find(ROOTSHELLS), Some(Ipv4Addr::new(192, 168, 1, 20)));
        assert!(local_dev_vpn(ROOTSHELLS));
        let text = unglued(ROOTSHELLS);
        let names: Vec<&str> = interfaces(&text).iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["lo0", "pdp_ip0", "ipsec2", "utun0", "en0", "utun5"]);
        // Its Wi-Fi's inet after a hwassist line, too.
        let glued = ROOTSHELLS.replace("\toptions=400<CHANNEL_IO>\n", "\thwassist=20000<VLAN_MTU>");
        assert_eq!(find(&glued), Some(Ipv4Addr::new(192, 168, 1, 20)));
        // Without LocalDevVPN's interface.
        let cut = ROOTSHELLS.find("utun5:").unwrap();
        assert!(!local_dev_vpn(&ROOTSHELLS[..cut]));
    }

    #[test]
    fn the_wifi_address_not_the_broadcast_one() {
        let text = "\tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255\n";
        assert_eq!(find(text), Some(Ipv4Addr::new(192, 168, 1, 20)));
    }
}
