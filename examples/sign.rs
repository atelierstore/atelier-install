//! What `atelier-install` does to an IPA, but for Apple's part, timed: gets
//! it (over HTTPS through rootshell's TLS, checked against its SHA-256),
//! unpacks it as isideload does, and signs it as isideload does, with a
//! certificate and key of the caller's in place of Apple's and the
//! profile's. To time the signing and check it under rootshell's runtime
//! (README.md), with the signing time fixed so two runs compare byte for
//! byte, and to count the signing's check-ins with rootshell, as the
//! installer makes them (src/awake.rs).
//!
//!     sign <link or path> <sha256> <key.der> <certificate.der>
//!
//! The key is PKCS#8 or PKCS#1 DER, the certificate DER. What it signs is
//! left in `.atelier/work`.

#[path = "../src/awake.rs"]
mod awake;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/blocking.rs"]
mod blocking;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/certificate.rs"]
mod certificate;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/handoff.rs"]
mod handoff;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/ipa.rs"]
mod ipa;
#[allow(
    dead_code,
    unused_imports,
    reason = "the installer's, which uses all of it"
)]
#[path = "../src/net.rs"]
mod net;
// File reads and writes in pieces rootshell can carry, and folders,
// renames and deletes as std expects them, as the installer's.
#[path = "../src/chunks.rs"]
mod chunks;
#[path = "../src/paths.rs"]
mod paths;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/state.rs"]
mod state;
#[allow(dead_code, reason = "the installer's, which uses all of it")]
#[path = "../src/terminal.rs"]
mod terminal;
#[path = "../src/vfs.rs"]
mod vfs;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use apple_codesign_quick::{BundleSigningSettings, RustCryptoCmsSigner, sign_bundle};
use isideload::sideload::application::Application;

/// The installer's, checking in with rootshell every so often.
#[global_allocator]
static ALLOCATOR: awake::Allocator = awake::Allocator;

/// A team ID for the entitlements, as Apple's look.
const TEAM: &str = "ABCDE12345";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [link, sha256, key, certificate] = arguments.as_slice() else {
        eprintln!("usage: sign <link or path> <sha256> <key.der> <certificate.der>");
        return ExitCode::from(2);
    };
    match sign(link, sha256, key, certificate) {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("{problem}");
            ExitCode::FAILURE
        }
    }
}

fn sign(link: &str, sha256: &str, key: &str, certificate: &str) -> Result<(), String> {
    let state = state::State::open().map_err(|error| error.to_string())?;
    isideload::http::set_connector(Arc::new(|host: &str, port: u16, tls: bool| {
        let stream = net::Stream::connect_host(host, port, tls)?;
        Ok(Box::new(stream) as Box<dyn isideload::http::Stream>)
    }));
    isideload_vfs::set_vfs(Box::new(vfs::StdVfs::new(state.work())));
    apple_codesign_quick::set_check_in(awake::check_in);

    let started = Instant::now();
    let ipa = ipa::get(&state, Some(link), Some(sha256))
        .map_err(|stop| format!("{}: {}", stop.what, stop.next.join(" ")))?;
    println!(
        "got {} {} ({}) in {:.2?}",
        ipa.name,
        ipa.version,
        ipa.bundle_id,
        started.elapsed()
    );

    let step = Instant::now();
    let app = Application::new(ipa.path.clone()).map_err(|report| format!("{report}"))?;
    println!(
        "unpacked {} in {:.2?}",
        app.bundle.bundle_dir.display(),
        step.elapsed()
    );

    let key = std::fs::read(key).map_err(|error| format!("{key}: {error}"))?;
    let certificate =
        std::fs::read(certificate).map_err(|error| format!("{certificate}: {error}"))?;
    let signer = RustCryptoCmsSigner::from_der(&key, &certificate, Vec::<Vec<u8>>::new())
        .map_err(|error| error.to_string())?
        .with_signing_time(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
    let mut entitlements = plist::Dictionary::new();
    entitlements.insert(
        "application-identifier".into(),
        format!("{TEAM}.{}.{TEAM}", ipa.bundle_id).into(),
    );
    entitlements.insert("com.apple.developer.team-identifier".into(), TEAM.into());
    entitlements.insert("get-task-allow".into(), true.into());
    let settings = BundleSigningSettings::new(TEAM, entitlements, Some(&signer));
    let step = Instant::now();
    let checked_in = awake::checked_in();
    sign_bundle(&app.bundle.bundle_dir, &settings).map_err(|error| error.to_string())?;
    println!(
        "signed in {:.2?}, checking in {} times",
        step.elapsed(),
        awake::checked_in() - checked_in
    );
    println!("all in {:.2?}", started.elapsed());
    Ok(())
}
