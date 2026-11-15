//! The program's calls to the iPhone it runs on: the RPPairing tunnel to
//! 10.7.0.1 through LocalDevVPN, AFC for the upload, installation_proxy for
//! the install and house_arrest to hand the app what it takes (handoff.rs),
//! such as Atelier's pairing file, on a tunnel that runs without a task of
//! its own (`tunnel.rs`).
//!
//! iOS picks the pairing service's port, and it can move after a restart.
//! The program asks Bonjour which devices offer it (`_remotepairing._tcp`)
//! and tries their ports at 10.7.0.1, where only this iPhone answers: the
//! ones at this iPhone's addresses first, then the port that worked last
//! and 49152, then the other devices'. From rootshell 1.0.16-177 on, a port
//! nothing listens at refuses at once. It never sweeps a range of ports.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use idevice::afc::opcode::AfcFopenMode;
use idevice::house_arrest::HouseArrestClient;
use idevice::lockdown::LockdownClient;
use idevice::remote_pairing::{
    RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::adapter::Adapter;
use idevice::{IdeviceError, RsdService};
use rootcause::Report;

use crate::blocking::{Blocking, NotReady, run};
use crate::net;
use crate::tunnel::Tunnel;
use crate::{handoff, terminal};

/// Where LocalDevVPN routes connections to the iPhone itself.
pub const DEVICE: Ipv4Addr = Ipv4Addr::new(10, 7, 0, 1);
/// The port iOS's RemotePairing service usually takes. iOS picks it, and it
/// can be another after a restart.
pub const DEFAULT_PORT: u16 = 49152;
/// The name the iPhone's logs give this host, as Atelier's.
const LABEL: &str = "Atelier";
/// The Bonjour service iOS's pairing service offers, and how long to look
/// for it: Bonjour answers within a second or so on a Wi-Fi network.
const SERVICE: &str = "_remotepairing._tcp";
const LOOK_FOR: std::time::Duration = std::time::Duration::from_secs(5);

/// The most ports the program tries that Bonjour gives.
const MOST_PORTS: usize = 8;

/// What Bonjour says of the pairing service: the ports of the devices at
/// one of `ours`, this iPhone's addresses, then the other devices', and a
/// line for the log without their addresses. Why not, otherwise.
pub fn find_ports(ours: &[IpAddr]) -> Result<(Vec<u16>, Vec<u16>, String), String> {
    let services = net::bonjour(SERVICE, LOOK_FOR).map_err(|error| error.to_string())?;
    if services.is_empty() {
        return Err("no device offers it".into());
    }
    let (mine, others) = split(&services, ours);
    Ok((mine, others, describe(&services, ours)))
}

fn is_ours(service: &net::Service, ours: &[IpAddr]) -> bool {
    service
        .addresses
        .iter()
        .any(|address| ours.contains(address))
}

/// The ports of the devices at one of `ours`, then the others', each once.
fn split(services: &[net::Service], ours: &[IpAddr]) -> (Vec<u16>, Vec<u16>) {
    let mut mine = Vec::new();
    let mut others = Vec::new();
    for service in services {
        let ports = if is_ours(service, ours) {
            &mut mine
        } else {
            &mut others
        };
        if !ports.contains(&service.port) {
            ports.push(service.port);
        }
    }
    others.retain(|port| !mine.contains(port));
    others.truncate(MOST_PORTS);
    mine.truncate(MOST_PORTS);
    (mine, others)
}

/// What Bonjour found, for the log: each device's port, how many addresses
/// of each kind it has, and whether one is this iPhone's. No addresses.
fn describe(services: &[net::Service], ours: &[IpAddr]) -> String {
    let each: Vec<String> = services
        .iter()
        .map(|service| {
            let v4 = service.addresses.iter().filter(|a| a.is_ipv4()).count();
            let v6 = service.addresses.len() - v4;
            let whose = if is_ours(service, ours) {
                "this iPhone's"
            } else {
                "not at this iPhone's addresses"
            };
            format!("port {} ({v4} IPv4, {v6} IPv6, {whose})", service.port)
        })
        .collect();
    format!(
        "{} device(s) offer it, {} address(es) known for this iPhone: {}",
        services.len(),
        ours.len(),
        each.join("; ")
    )
}

#[derive(Debug)]
pub enum Problem {
    /// Nothing answers at 10.7.0.1: LocalDevVPN isn't connected.
    NoVpn(String),
    /// 10.7.0.1 answers, but not at that port.
    NoService(String),
    /// The iPhone doesn't know this pairing file.
    NotPaired,
    Other(String),
}

pub struct Phone {
    tunnel: Tunnel,
    handshake: RsdHandshake,
    pub udid: String,
    pub name: String,
    pub ios: Option<String>,
}

/// Runs `future`, one of idevice's steps, to its end.
fn step<T>(
    what: &str,
    future: impl Future<Output = Result<T, IdeviceError>>,
) -> Result<T, Problem> {
    match run(future) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Problem::Other(format!("{what}: {error}"))),
        Err(NotReady) => Err(Problem::Other(format!(
            "{what}: it waited for something rootshell's programs don't have, such as a timer"
        ))),
    }
}

/// Opens the tunnel through the pairing service at `port`.
pub fn connect(pairing: &mut RpPairingFile, port: u16) -> Result<Phone, Problem> {
    let stream = net::Stream::connect(DEVICE, port).map_err(|error| match error.kind() {
        std::io::ErrorKind::ConnectionRefused => Problem::NoService(error.to_string()),
        _ => Problem::NoVpn(error.to_string()),
    })?;

    let mut client = RemotePairingClient::new(RpPairingSocket::new(Blocking(stream)), LABEL);
    step("pairing service", client.attempt_pair_verify())?;
    // Not `connect`, which pairs anew when the iPhone doesn't know the
    // file: that's step 1's, with its code.
    match run(client.validate_pairing(pairing)) {
        Ok(Ok(())) => {}
        Ok(Err(IdeviceError::RemotePairing(error))) => {
            terminal::log(format!("pairing refused: {error:?}"));
            return Err(Problem::NotPaired);
        }
        Ok(Err(error)) => return Err(Problem::Other(format!("pairing: {error}"))),
        Err(NotReady) => return Err(Problem::Other("pairing: it waited for a timer".into())),
    }

    let tunnel_port = step("tunnel", client.create_tcp_listener())?;
    let tunnel_stream = net::Stream::connect(DEVICE, tunnel_port)
        .map_err(|error| Problem::Other(format!("tunnel connection: {error}")))?;
    let cdtunnel = step(
        "tunnel handshake",
        connect_tls_psk_tunnel_native(Blocking(tunnel_stream), client.encryption_key()),
    )?;
    let address = |text: &str| {
        text.parse::<IpAddr>()
            .map_err(|error| Problem::Other(format!("tunnel address {text}: {error}")))
    };
    let client_ip = address(&cdtunnel.info.client_address)?;
    let server_ip = address(&cdtunnel.info.server_address)?;
    let mtu = cdtunnel.info.mtu as usize;
    let rsd_port = cdtunnel.info.server_rsd_port;
    let mut adapter = Adapter::new(Box::new(cdtunnel.into_inner()), client_ip, server_ip);
    adapter.set_mss(mtu.saturating_sub(60));
    let mut tunnel = Tunnel::new(adapter);

    let rsd = tunnel
        .connect(rsd_port)
        .map_err(|error| Problem::Other(format!("services: {error}")))?;
    let mut handshake = step("services", RsdHandshake::new(rsd))?;

    let mut lockdown = step(
        "lockdown",
        LockdownClient::connect_rsd(&mut tunnel, &mut handshake),
    )?;
    let mut value = |key: &str| {
        let value = run(lockdown.get_value(Some(key), None)).ok()?.ok()?;
        value.as_string().map(str::to_owned)
    };
    let udid = match handshake
        .properties
        .get("UniqueDeviceID")
        .and_then(|value| value.as_string())
    {
        Some(udid) => udid.to_owned(),
        None => value("UniqueDeviceID")
            .ok_or_else(|| Problem::Other("the iPhone didn't say its identifier".into()))?,
    };
    let name = value("DeviceName").unwrap_or_else(|| "iPhone".into());
    let ios = value("ProductVersion");
    drop(lockdown);

    Ok(Phone {
        tunnel,
        handshake,
        udid,
        name,
        ios,
    })
}

impl Phone {
    /// Uploads the signed app to PublicStaging and installs it, as a
    /// developer's app: `percent` goes from 0 to 100.
    pub fn install(
        &mut self,
        app: &Path,
        percent: impl Fn(u64) + Send + Sync,
    ) -> Result<(), Report> {
        let installing = isideload::sideload::install::install_app_rsd(
            &mut self.tunnel,
            &mut self.handshake,
            app,
            percent,
        );
        run(installing).unwrap_or_else(|NotReady| {
            Err(rootcause::report!(
                "the install waited for something rootshell's programs don't have, such as a timer"
            ))
        })
    }

    /// Writes `file` at `path` in the container of the app `bundle_id`, as
    /// it was just installed, making its folders first: a hand-off
    /// (handoff.rs).
    pub fn give(&mut self, bundle_id: &str, path: &str, file: &[u8]) -> Result<(), IdeviceError> {
        let giving = async {
            let house_arrest =
                HouseArrestClient::connect_rsd(&mut self.tunnel, &mut self.handshake).await?;
            let mut container = house_arrest.vend_container(bundle_id).await?;
            for folder in handoff::folders(path) {
                // Fails harmlessly when the folder is already there.
                let _ = container.mk_dir(folder).await;
            }
            let mut handle = container.open(path, AfcFopenMode::WrOnly).await?;
            handle.write_entire(file).await?;
            handle.close().await
        };
        run(giving).unwrap_or(Err(IdeviceError::UnexpectedResponse(
            "it waited for something rootshell's programs don't have, such as a timer".into(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_this_iphones_ports_first_of_what_bonjour_finds() {
        let service = |port, addresses: &[&str]| net::Service {
            port,
            addresses: addresses.iter().map(|a| a.parse().unwrap()).collect(),
        };
        let found = [
            service(62078, &["192.168.1.30"]),
            service(52143, &["fe80::1c2d:3e4f:5a6b:7c8d"]),
            service(49153, &["192.168.1.20", "fe80::1"]),
            service(62078, &["192.168.1.31"]),
        ];
        let ours: Vec<IpAddr> = ["192.168.1.20".parse().unwrap()].into();
        assert_eq!(split(&found, &ours), (vec![49153], vec![62078, 52143]));
        // Only by IPv6, as when Bonjour lists the iPhone's link-local address.
        let ours: Vec<IpAddr> = ["fe80::1c2d:3e4f:5a6b:7c8d".parse().unwrap()].into();
        assert_eq!(split(&found, &ours), (vec![52143], vec![62078, 49153]));
        // Without this iPhone's addresses, every port, as found.
        assert_eq!(split(&found, &[]), (vec![], vec![62078, 52143, 49153]));
        assert_eq!(split(&[], &ours), (vec![], vec![]));
        let said = describe(&found[1..3], &ours);
        assert_eq!(
            said,
            "2 device(s) offer it, 1 address(es) known for this iPhone: \
             port 52143 (0 IPv4, 1 IPv6, this iPhone's); \
             port 49153 (1 IPv4, 1 IPv6, not at this iPhone's addresses)"
        );
        assert!(!said.contains("192.168") && !said.contains("fe80"));
    }
}
