//! The Bonjour announcement of a pairable host, built by hand.
//!
//! iOS 27 lists a host under Settings › Privacy & Security › Developer Mode
//! as "Pair with …" when the host announces `_remotepairing-pairable-host._tcp`
//! on the local network. An app may only announce the service types its
//! Info.plist declares, which is how Atelier itself does it; rootshell
//! declares others, but it has Apple's multicast entitlement, so a program it
//! runs can send the announcement itself, as a multicast DNS packet (RFC 6762
//! and 6763).
//!
//! The program can't answer the questions other devices then ask: rootshell
//! lets it wait for one thing at a time, and it waits for the iPhone to
//! connect. So the announcement has to say everything, and iOS answers
//! Settings from what it kept of it: the service, its port, its TXT record,
//! and the address of the host name it points to, each kept for [`TTL`].

use std::net::Ipv4Addr;

/// Where multicast DNS is sent.
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;

/// The service iOS 27 looks for.
pub const SERVICE: &str = "_remotepairing-pairable-host._tcp";

/// How long iOS keeps the announcement: 75 minutes, the longest Bonjour
/// itself uses. Past that the "Pair with …" entry goes, and the program has
/// to be run again.
pub const TTL: u32 = 75 * 60;

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
/// Set on records only this host has: whoever kept an older one drops it.
const CACHE_FLUSH: u16 = 0x8000;

/// A pairable host to announce.
#[derive(Debug, Clone)]
pub struct Host {
    /// The service's instance name: the host's pairing identifier.
    pub instance: String,
    /// The host name the service points to, without `.local`.
    pub host_label: String,
    pub address: Ipv4Addr,
    pub port: u16,
    /// The TXT record's `key=value` pairs.
    pub txt: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// A name's label is empty or longer than DNS's 63 bytes.
    Label(String),
    /// A TXT pair is longer than DNS's 255 bytes.
    Txt(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Label(label) => write!(f, "`{label}` can't be part of a Bonjour name"),
            Problem::Txt(key) => write!(f, "the TXT entry `{key}` is longer than 255 bytes"),
        }
    }
}

impl Host {
    /// The packet that announces the host.
    pub fn announcement(&self) -> Result<Vec<u8>, Problem> {
        self.packet(TTL)
    }

    /// The packet that takes the announcement back: the same records, to be
    /// kept for no time at all.
    pub fn goodbye(&self) -> Result<Vec<u8>, Problem> {
        self.packet(0)
    }

    fn packet(&self, ttl: u32) -> Result<Vec<u8>, Problem> {
        let service = [SERVICE, "local"].join(".");
        let service: Vec<&str> = service.split('.').collect();
        let mut instance = vec![self.instance.as_str()];
        instance.extend(&service);
        let host = [self.host_label.as_str(), "local"];

        let mut txt = Vec::new();
        for (key, value) in &self.txt {
            let entry = format!("{key}={value}");
            let len = u8::try_from(entry.len()).map_err(|_| Problem::Txt(key.clone()))?;
            txt.push(len);
            txt.extend_from_slice(entry.as_bytes());
        }
        if txt.is_empty() {
            // An empty TXT record is a single empty string.
            txt.push(0);
        }

        let mut srv = vec![0, 0, 0, 0]; // priority, weight
        srv.extend_from_slice(&self.port.to_be_bytes());
        srv.extend(name(&host)?);

        let records = [
            // The service type among those this network has, for browsers
            // that list them all.
            record(
                &["_services", "_dns-sd", "_udp", "local"],
                TYPE_PTR,
                CLASS_IN,
                ttl,
                &name(&service)?,
            )?,
            record(&service, TYPE_PTR, CLASS_IN, ttl, &name(&instance)?)?,
            record(&instance, TYPE_SRV, CLASS_IN | CACHE_FLUSH, ttl, &srv)?,
            record(&instance, TYPE_TXT, CLASS_IN | CACHE_FLUSH, ttl, &txt)?,
            record(
                &host,
                TYPE_A,
                CLASS_IN | CACHE_FLUSH,
                ttl,
                &self.address.octets(),
            )?,
        ];

        // The header: no ID, an authoritative answer, and only answers.
        let mut packet = vec![0, 0, 0x84, 0, 0, 0];
        packet.extend_from_slice(&(records.len() as u16).to_be_bytes());
        packet.extend_from_slice(&[0, 0, 0, 0]);
        for record in records {
            packet.extend(record);
        }
        Ok(packet)
    }
}

/// A name in DNS wire format: each label after its length, then the root.
/// Written out whole every time, as compression is optional.
fn name(labels: &[&str]) -> Result<Vec<u8>, Problem> {
    let mut out = Vec::new();
    for label in labels {
        if label.is_empty() || label.len() > 63 {
            return Err(Problem::Label((*label).to_string()));
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(out)
}

fn record(
    owner: &[&str],
    kind: u16,
    class: u16,
    ttl: u32,
    data: &[u8],
) -> Result<Vec<u8>, Problem> {
    let mut out = name(owner)?;
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host {
            instance: "6e3199df-1693-38fa-882c-048ff41f6818".into(),
            host_label: "atelier-0a1b2c3d".into(),
            address: Ipv4Addr::new(192, 168, 1, 20),
            port: 50123,
            txt: vec![
                ("name".into(), "Atelier".into()),
                (
                    "identifier".into(),
                    "6e3199df-1693-38fa-882c-048ff41f6818".into(),
                ),
                ("authTag".into(), "NP0m1CHW".into()),
                ("model".into(), "Mac17,7".into()),
                ("flags".into(), "1".into()),
                ("ver".into(), "26".into()),
                ("minVer".into(), "17".into()),
            ],
        }
    }

    /// What a DNS reader makes of a packet's answers: each as its owner,
    /// type, class, TTL and data.
    fn answers(packet: &[u8]) -> Vec<(String, u16, u16, u32, Vec<u8>)> {
        fn read_name(packet: &[u8], at: &mut usize) -> String {
            let mut labels = Vec::new();
            loop {
                let len = packet[*at] as usize;
                *at += 1;
                if len == 0 {
                    return labels.join(".");
                }
                assert!(len < 64, "no compression in these packets");
                labels.push(String::from_utf8(packet[*at..*at + len].to_vec()).unwrap());
                *at += len;
            }
        }
        let u16_at = |at: usize| u16::from_be_bytes([packet[at], packet[at + 1]]);
        assert_eq!(&packet[..4], [0, 0, 0x84, 0], "an authoritative response");
        assert_eq!((u16_at(4), u16_at(8), u16_at(10)), (0, 0, 0));
        let mut at = 12;
        let mut out = Vec::new();
        for _ in 0..u16_at(6) {
            let owner = read_name(packet, &mut at);
            let (kind, class) = (u16_at(at), u16_at(at + 2));
            let ttl = u32::from_be_bytes(packet[at + 4..at + 8].try_into().unwrap());
            let len = u16_at(at + 8) as usize;
            out.push((
                owner,
                kind,
                class,
                ttl,
                packet[at + 10..at + 10 + len].to_vec(),
            ));
            at += 10 + len;
        }
        assert_eq!(at, packet.len(), "nothing after the answers");
        out
    }

    #[test]
    fn the_announcement_says_everything_settings_asks() {
        let host = host();
        let packet = host.announcement().unwrap();
        let answers = answers(&packet);
        let instance =
            "6e3199df-1693-38fa-882c-048ff41f6818._remotepairing-pairable-host._tcp.local";
        let owners: Vec<_> = answers.iter().map(|a| (a.0.as_str(), a.1)).collect();
        assert_eq!(
            owners,
            [
                ("_services._dns-sd._udp.local", TYPE_PTR),
                ("_remotepairing-pairable-host._tcp.local", TYPE_PTR),
                (instance, TYPE_SRV),
                (instance, TYPE_TXT),
                ("atelier-0a1b2c3d.local", TYPE_A),
            ]
        );
        assert!(answers.iter().all(|a| a.3 == TTL));

        // The service's instance, then its port and host, then the host's address.
        assert_eq!(
            answers[1].4,
            name(&instance.split('.').collect::<Vec<_>>()).unwrap()
        );
        let srv = &answers[2].4;
        assert_eq!(srv[..4], [0, 0, 0, 0]);
        assert_eq!(u16::from_be_bytes([srv[4], srv[5]]), 50123);
        assert_eq!(srv[6..], name(&["atelier-0a1b2c3d", "local"]).unwrap());
        assert_eq!(answers[4].4, [192, 168, 1, 20]);

        // Shared records keep others' answers; this host's own replace them.
        assert_eq!(answers[0].2, CLASS_IN);
        assert_eq!(answers[1].2, CLASS_IN);
        for unique in &answers[2..] {
            assert_eq!(unique.2, CLASS_IN | CACHE_FLUSH);
        }
    }

    #[test]
    fn the_txt_record_is_each_pair_after_its_length() {
        let packet = host().announcement().unwrap();
        let txt = &answers(&packet)[3].4;
        let mut pairs = Vec::new();
        let mut at = 0;
        while at < txt.len() {
            let len = txt[at] as usize;
            pairs.push(String::from_utf8(txt[at + 1..at + 1 + len].to_vec()).unwrap());
            at += 1 + len;
        }
        assert_eq!(
            pairs,
            [
                "name=Atelier",
                "identifier=6e3199df-1693-38fa-882c-048ff41f6818",
                "authTag=NP0m1CHW",
                "model=Mac17,7",
                "flags=1",
                "ver=26",
                "minVer=17",
            ]
        );
    }

    #[test]
    fn the_goodbye_is_the_announcement_kept_for_no_time() {
        let host = host();
        let (hello, bye) = (
            answers(&host.announcement().unwrap()),
            answers(&host.goodbye().unwrap()),
        );
        assert_eq!(hello.len(), bye.len());
        for (hello, bye) in hello.iter().zip(&bye) {
            assert_eq!(
                (&hello.0, hello.1, hello.2, &hello.4),
                (&bye.0, bye.1, bye.2, &bye.4)
            );
            assert_eq!(bye.3, 0);
        }
    }

    #[test]
    fn it_fits_one_packet() {
        // Under what a Wi-Fi network carries in one piece, with room to spare.
        assert!(host().announcement().unwrap().len() < 1200);
    }

    #[test]
    fn names_and_txt_entries_dns_cant_carry_are_refused() {
        let mut long_name = host();
        long_name.instance = "x".repeat(64);
        assert_eq!(
            long_name.announcement(),
            Err(Problem::Label("x".repeat(64)))
        );
        let mut no_name = host();
        no_name.host_label = String::new();
        assert_eq!(no_name.announcement(), Err(Problem::Label(String::new())));
        let mut long_txt = host();
        long_txt.txt.push(("name".into(), "x".repeat(255)));
        assert_eq!(long_txt.announcement(), Err(Problem::Txt("name".into())));
    }
}
