//! The pairing with this iPhone: the one kept from an earlier run, one
//! Atelier's pairing program (`atelier-pair.wasm`) left in rootshell's
//! folder, or a new one, made the way that program makes it:
//! announce a pairable host on the Wi-Fi, and pair with the iPhone once the
//! user picks it in Settings and types the code shown here. No transfer
//! code: the program that pairs is the one that installs.

use std::cell::Cell;
use std::fs;
use std::io::{BufRead, Write};
use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use idevice::remote_pairing::{PairableHost, PairableHostInfo, RpPairingFile, RpPairingSocket};

use crate::blocking::{self, Blocking};
use crate::net::{self, Listener};
use crate::state::{self, State};
use crate::terminal::{self, Stop};
use crate::{address, mdns};

/// The pairing program's name for it, in rootshell's folder.
pub const FILE_NAME: &str = "atelier-pairing.plist";
/// What Settings lists: "Pair with Atelier".
const DISPLAY_NAME: &str = "Atelier";
/// iOS treats a pairable host as a computer; this Mac is idevice's default,
/// and what Atelier's own host says.
const HOST_MODEL: &str = "Mac17,7";
/// `ifconfig en0 > ifconfig.txt`, the address's usual source.
const IFCONFIG_FILE: &str = "ifconfig.txt";
/// How many connections one run pairs on: a stray one, or a mistyped code,
/// leaves the entry in Settings good for another tap.
const MAX_CONNECTIONS: u32 = 5;
/// Announcements, a second apart, as Bonjour sends its own.
const ANNOUNCEMENTS: u32 = 3;

pub struct Pairing {
    pub file: RpPairingFile,
    /// As it was read, to hand to Atelier once it's installed.
    pub bytes: Vec<u8>,
}

/// The pairing kept from an earlier run, or the pairing program's, kept
/// from here on; None when there's neither.
pub fn find(state: &State) -> Result<Option<Pairing>, Stop> {
    let kept = state.pairing();
    if let Ok(bytes) = fs::read(&kept) {
        match RpPairingFile::from_bytes(&bytes) {
            Ok(file) => {
                terminal::say("Using the pairing kept from the last run.");
                return Ok(Some(Pairing { file, bytes }));
            }
            Err(_) => {
                let _ = fs::remove_file(&kept);
                terminal::say("The pairing kept from the last run can't be read, so it's deleted.");
            }
        }
    }

    let left = state::files_folder().join(FILE_NAME);
    let Ok(bytes) = fs::read(&left) else {
        return Ok(None);
    };
    let file = RpPairingFile::from_bytes(&bytes).map_err(|_| {
        Stop::new(format!(
            "{FILE_NAME} in rootshell's folder isn't a pairing file"
        ))
        .next("Delete it in Files, then run this again to pair anew.")
    })?;
    state::write_new(&kept, &bytes)?;
    // Files shows rootshell's folder: the key to this iPhone doesn't stay
    // where it's in view.
    let _ = fs::remove_file(&left);
    terminal::say(format!("Took {FILE_NAME} from the pairing program."));
    Ok(Some(Pairing { file, bytes }))
}

/// Pairs this iPhone with the program, and keeps the pairing.
pub fn pair(state: &State, address: Option<&str>) -> Result<Pairing, Stop> {
    let address = wifi_address(address)?;
    let port = random_port();
    let listener = Listener::bind(port)
        .map_err(|e| Stop::new(format!("Couldn't listen for the iPhone: {e}")))?;

    let mut identity = RpPairingFile::generate(&unique_seed());
    let info = PairableHostInfo::generate(DISPLAY_NAME, HOST_MODEL);
    let host = mdns::Host {
        instance: identity.identifier.clone(),
        host_label: format!("atelier-{:08x}", random_u32()),
        address,
        port,
        txt: info.mdns_txt_records(&identity.identifier),
    };
    announce(&host)?;

    println!();
    terminal::say("Now, on this iPhone:");
    terminal::say("  1. Open Settings › Privacy & Security › Developer Mode.");
    terminal::say(format!(
        "  2. Scroll down and tap \"Pair with {DISPLAY_NAME}\"."
    ));
    terminal::say("  3. Enter your passcode, then the code that will show here.");
    terminal::say("Leave rootshell running meanwhile. It keeps going behind Settings");
    terminal::say("with its Settings › Location Diary Mode set to Session Only.");
    terminal::say(format!(
        "The entry stays in Settings for {} minutes.",
        mdns::TTL / 60
    ));

    let paired = accept(&listener, &info, &mut identity);
    // Whatever came of it, the entry in Settings has done its job.
    if let Ok(goodbye) = host.goodbye() {
        let _ = net::send_datagram(mdns::GROUP, mdns::PORT, &goodbye);
    }
    drop(listener);
    let device = paired?;
    terminal::say(format!("Paired with {device}."));

    let bytes = identity.to_bytes();
    state::write_new(&state.pairing(), &bytes)
        .map_err(|e| Stop::new(format!("Paired, but couldn't keep the pairing: {e}")))?;
    Ok(Pairing {
        file: identity,
        bytes,
    })
}

/// This iPhone's Wi-Fi address: as given, from `ifconfig.txt`, or as typed.
fn wifi_address(given: Option<&str>) -> Result<Ipv4Addr, Stop> {
    if let Some(given) = given {
        return address::find(given).ok_or_else(|| {
            Stop::new(format!("`{given}` isn't an address like 192.168.1.20"))
                .next("It's in Settings › Wi-Fi › ⓘ › IP Address.")
        });
    }
    let ifconfig = state::files_folder().join(IFCONFIG_FILE);
    match fs::read_to_string(&ifconfig) {
        Ok(text) => match address::find(&text) {
            Some(address) => {
                terminal::say(format!(
                    "This iPhone's Wi-Fi address is {address}, from {IFCONFIG_FILE}."
                ));
                return Ok(address);
            }
            None => terminal::say(format!(
                "{IFCONFIG_FILE} has no Wi-Fi address in it. Is Wi-Fi on and connected?"
            )),
        },
        Err(_) => terminal::say(format!(
            "There's no {IFCONFIG_FILE} here (ifconfig en0 > {IFCONFIG_FILE} makes it)."
        )),
    }
    terminal::say("Type this iPhone's Wi-Fi address, from Settings › Wi-Fi › ⓘ › IP Address:");
    let _ = std::io::stdout().flush();
    let mut read = String::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        read.push_str(&line);
        read.push('\n');
        if let Some(address) = address::find(&line).or_else(|| address::find(&read)) {
            return Ok(address);
        }
        if read.lines().count() >= 64 {
            break;
        }
    }
    Err(
        Stop::new("No Wi-Fi address, so there's nothing to announce")
            .next("Connect to Wi-Fi, then run this again."),
    )
}

/// Sends the announcement, [`ANNOUNCEMENTS`] times.
fn announce(host: &mdns::Host) -> Result<(), Stop> {
    let packet = host
        .announcement()
        .map_err(|e| Stop::new(format!("Couldn't write the announcement: {e}")))?;
    let mut sent = 0;
    let mut last_problem = None;
    for round in 0..ANNOUNCEMENTS {
        if round > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        match net::send_datagram(mdns::GROUP, mdns::PORT, &packet) {
            Ok(()) => sent += 1,
            Err(e) => last_problem = Some(e),
        }
    }
    match (sent, last_problem) {
        (0, Some(e)) => Err(Stop::new(format!("Couldn't announce on the local network: {e}"))
            .next("Check that rootshell may use the local network (Settings › Apps › rootshell › Local Network) and that Wi-Fi is on.")),
        _ => {
            terminal::say(format!(
                "Announced \"{DISPLAY_NAME}\" at {}, port {}.",
                host.address, host.port
            ));
            Ok(())
        }
    }
}

/// Pairs with the iPhone on the first connection that gets through
/// pair-setup, and returns it as "name (model)".
fn accept(
    listener: &Listener,
    info: &PairableHostInfo,
    identity: &mut RpPairingFile,
) -> Result<String, Stop> {
    for connection in 1..=MAX_CONNECTIONS {
        let stream = listener
            .accept()
            .map_err(|e| Stop::new(format!("Stopped waiting for the iPhone: {e}")))?;
        let code_shown = Cell::new(false);
        let mut host =
            PairableHost::new(RpPairingSocket::new_device(Blocking(stream)), info.clone());
        let outcome = blocking::run(host.accept(identity, |code| {
            code_shown.set(true);
            show_code("Code for Settings", &code);
            async {}
        }));
        match outcome {
            Ok(Ok(device)) => return Ok(format!("{} ({})", device.name, device.model)),
            Ok(Err(e)) if !code_shown.get() => {
                terminal::say(format!("A connection ended before pairing started ({e})."));
            }
            Ok(Err(e)) => {
                terminal::say(format!(
                    "That didn't pair ({e}). Was the code typed as shown?"
                ));
            }
            Err(blocking::NotReady) => {
                return Err(Stop::new(
                    "Pairing waited for something this program can't wait for. That's a bug in it",
                ));
            }
        }
        if connection < MAX_CONNECTIONS {
            terminal::say(format!(
                "Still waiting: tap \"Pair with {DISPLAY_NAME}\" again for a new code."
            ));
        }
    }
    Err(
        Stop::new(format!("{MAX_CONNECTIONS} connections didn't pair"))
            .next("Run this again to start over."),
    )
}

/// Shows a code in the terminal, and as a notification where rootshell
/// sends those (its Settings › Terminal Notifications), for when the user is
/// in Settings: the terminal takes OSC 777 for one and prints nothing.
fn show_code(what: &str, code: &str) {
    let code = spaced(code);
    println!("    {what}: {code}");
    terminal::notify(&format!("{what}: {code}"));
}

/// A 6-digit code as people read one: "123 456".
fn spaced(code: &str) -> String {
    match code.len() {
        6 if code.is_ascii() => format!("{} {}", &code[..3], &code[3..]),
        _ => code.to_string(),
    }
}

fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes).expect("the system's random source");
    u32::from_le_bytes(bytes)
}

/// A port for the pairing listener. rootshell can't say which port a
/// listener got, so the program picks: one of the upper ports nothing is
/// registered on, away from the iPhone's own pairing service (49152 on).
fn random_port() -> u16 {
    52000 + (random_u32() % 12000) as u16
}

/// A seed for idevice's name-based pairing identifier that differs on every
/// run, so the iPhone sees each as a new host, as with Atelier's own.
fn unique_seed() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{DISPLAY_NAME}/{nanos}/{:08x}{:08x}",
        random_u32(),
        random_u32()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_in_threes() {
        assert_eq!(spaced("123456"), "123 456");
        assert_eq!(spaced("12345"), "12345");
    }

    #[test]
    fn ports_stay_clear_of_the_iphones() {
        for _ in 0..2000 {
            assert!((52000..64000).contains(&random_port()));
        }
    }

    #[test]
    fn every_run_is_a_new_host() {
        let (a, b) = (
            RpPairingFile::generate(&unique_seed()),
            RpPairingFile::generate(&unique_seed()),
        );
        assert_ne!(a.identifier, b.identifier);
    }
}
