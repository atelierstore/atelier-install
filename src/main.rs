//! Pairs this iPhone, signs an app with an Apple ID and installs it, all
//! from rootshell (App Store), in one WebAssembly program: no computer and
//! no second app. It
//!
//! 1. pairs with the iPhone, unless an earlier run or Atelier's pairing
//!    program did (`pairing.rs`),
//! 2. opens a tunnel to it through LocalDevVPN (`phone.rs`), on a TCP
//!    stack it drives step by step (`tunnel.rs`),
//! 3. gets the IPA (`ipa.rs`),
//! 4. signs in to Apple with the Apple ID (`apple.rs`), through the
//!    anisette servers by Atelier's rules (`route.rs`), or with Apple's
//!    token from the last sign-in, without the password (`token.rs`),
//! 5. signs the app for this iPhone, once the user said yes to what it
//!    takes from the Apple ID,
//! 6. installs it as a developer's app, the way Xcode does, and
//! 7. hands the app what it takes (`handoff.rs`): what its Info.plist
//!    declares, once the user said yes, or, for Atelier, the pairing, and,
//!    when Atelier takes them, the certificate it's signed with and the
//!    sign-in, so that it opens signed in (`certificate.rs`).
//!    It deletes its own copy of the pairing once the app has it.
//!
//! What a step makes is kept until the install's done (`kept.rs`), so a run
//! stopped on the way, by a connection that broke or Control-C, goes on
//! from where it stopped: the same IPA isn't downloaded or signed again.
//!
//! rootshell's programs have one thread, blocking sockets and no async
//! runtime, so each of idevice's and isideload's async steps runs to its end
//! at once, on sockets that are always ready (`blocking.rs`). They run in a
//! web view in no window, whose work WebKit can suspend while the program
//! asks rootshell nothing, so this one checks in as it works (`awake.rs`).
//! isideload's HTTPS goes through rootshell's own TLS, Apple's, on
//! connections this program hands it (`net.rs`, vendor/isideload), and
//! its files through the system's (`vfs.rs`).

mod address;
mod apple;
mod awake;
mod blocking;
mod certificate;
mod handoff;
mod ipa;
mod kept;
mod mdns;
mod net;
mod pairing;
mod phone;
// File reads and writes in pieces rootshell can carry, and folders,
// renames and deletes with the errors std expects, for std and every
// crate.
mod chunks;
mod paths;
mod route;
mod state;
mod terminal;
#[cfg(test)]
mod testing;
mod token;
mod tunnel;
mod vfs;

use std::fs;
use std::future::Future;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use isideload::dev::developer_session::DeveloperSession;

use crate::handoff::HandOffs;
use crate::pairing::Pairing;
use crate::state::State;
use crate::terminal::Stop;

/// The system's, checking in with rootshell every so often (`awake.rs`).
#[global_allocator]
static ALLOCATOR: awake::Allocator = awake::Allocator;

const USAGE: &str = "\
atelier-install: pairs this iPhone, signs an app with your Apple ID and
installs it, all from rootshell.

Before it: turn Developer Mode on, be on Wi-Fi, and have LocalDevVPN.

  ./atelier-install.wasm                  install the IPA in rootshell's folder
  ./atelier-install.wasm --ipa <where>    install the IPA at a path or an https link
  ./atelier-install.wasm --check          only pair, and check LocalDevVPN
  ./atelier-install.wasm --sign-in        only check that Apple takes your Apple ID

  --sha256 <hex>      the IPA's SHA-256, to check it against
  --address <ip>      this iPhone's Wi-Fi address, for pairing, in place of
                      ifconfig.txt (ifconfig en0 > ifconfig.txt makes it)
  --port <number>     the iPhone's pairing service's port, when it isn't 49152
  --anisette <link>   your own anisette server, in place of Atelier's list
  --pairing-to <path> where the app takes this iPhone's pairing, in its
                      container, when its Info.plist doesn't say (it's asked)
  --redo              sign the IPA anew, where a run that stopped would go on
  --forget            delete what this program keeps in .atelier
";

/// The steps of a whole install.
const STEPS: u32 = 7;

#[derive(Default, Debug, PartialEq)]
struct Options {
    ipa: Option<String>,
    sha256: Option<String>,
    address: Option<String>,
    port: Option<u16>,
    anisette: Option<String>,
    pairing_to: Option<String>,
    redo: bool,
    check: bool,
    sign_in: bool,
    forget: bool,
    help: bool,
}

fn parse(arguments: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| arguments.next().ok_or(format!("{name} needs a value"));
        match argument.as_str() {
            "-h" | "--help" | "help" => options.help = true,
            "--check" => options.check = true,
            "--sign-in" => options.sign_in = true,
            "--forget" => options.forget = true,
            "--redo" => options.redo = true,
            "--ipa" => options.ipa = Some(value("--ipa")?),
            "--sha256" => {
                let digest = value("--sha256")?;
                if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err("--sha256 takes 64 hexadecimal characters".into());
                }
                options.sha256 = Some(digest);
            }
            "--address" => options.address = Some(value("--address")?),
            "--port" => {
                options.port = Some(
                    value("--port")?
                        .parse()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or("--port takes a number from 1 to 65535")?,
                );
            }
            "--anisette" => {
                let link = value("--anisette")?;
                if !link.starts_with("https://") {
                    return Err("--anisette takes an https link".into());
                }
                options.anisette = Some(link);
            }
            "--pairing-to" => {
                let path = value("--pairing-to")?;
                options.pairing_to = Some(handoff::container_path(&path).map_err(|_| {
                    "--pairing-to takes a path in the app's container, such as Documents/pairingFile.plist"
                })?);
            }
            other => return Err(format!("`{other}` isn't an option")),
        }
    }
    Ok(options)
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(problem) => {
            eprintln!("{problem}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if options.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let result = State::open().map_err(Stop::from).and_then(|state| {
        if options.forget {
            state.forget()?;
            println!(
                "Deleted what this program kept: the pairing, the signing key and the sign-in servers' states."
            );
            return Ok(());
        }
        terminal::start_log(&state::files_folder().join(LOG));
        terminal::log(format!("atelier-install {}", env!("CARGO_PKG_VERSION")));
        prepare(&state);
        let result = run(&options, &state);
        // The IPA as it was unpacked and signed, unless it waits for the
        // next run to install it.
        kept::tidy(&state);
        if result.is_err() {
            println!("\nThe steps and errors of this run are in {LOG}, in rootshell's folder.");
        }
        result
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(stop) => {
            terminal::log(format!("stopped: {}", stop.what));
            eprintln!("\n{}.", stop.what.trim_end_matches('.'));
            for line in &stop.next {
                eprintln!("  {line}");
            }
            ExitCode::FAILURE
        }
    }
}

/// The log, in the folder Files shows, to send along when a run fails.
const LOG: &str = "atelier-install.log";

/// Hands isideload what rootshell has in place of what it'd take from the
/// system: connections, with rootshell's TLS, and files, with its
/// temporary folder in `.atelier`. Its signing checks in as it hashes.
fn prepare(state: &State) {
    isideload::http::set_connector(Arc::new(|host: &str, port: u16, tls: bool| {
        let stream = net::Stream::connect_host(host, port, tls)?;
        Ok(Box::new(stream) as Box<dyn isideload::http::Stream>)
    }));
    isideload_vfs::set_vfs(Box::new(vfs::StdVfs::new(state.work())));
    apple_codesign_quick::set_check_in(awake::check_in);
    let _ = isideload::init();
}

/// Runs one of isideload's async steps to its end. All it waits for is
/// rootshell's sockets, which are always ready.
fn wait<T>(step: impl Future<Output = Result<T, Stop>>) -> Result<T, Stop> {
    blocking::run(step).unwrap_or_else(|blocking::NotReady| {
        Err(Stop::new(
            "A step waited for something rootshell's programs can't wait for. That's a bug in this program",
        ))
    })
}

fn run(options: &Options, state: &State) -> Result<(), Stop> {
    println!("atelier-install, a test build. It pairs this iPhone, signs an app with");
    println!("your Apple ID and installs it.");
    if options.sign_in {
        terminal::step(1, 1, "Your Apple ID");
        sign_in(options, state, None)?;
        println!("\nApple takes this Apple ID from here. Nothing was signed or installed.");
        return Ok(());
    }
    let steps = if options.check { 2 } else { STEPS };

    terminal::step(1, steps, "The pairing with this iPhone");
    let (mut pairing, mut paired_now) = match pairing::find(state)? {
        Some(pairing) => (pairing, false),
        None => {
            terminal::say("This iPhone isn't paired with this program yet, so it pairs now.");
            (pairing::pair(state, options.address.as_deref())?, true)
        }
    };

    terminal::step(2, steps, "This iPhone, through LocalDevVPN");
    if paired_now {
        vpn_on()?;
    }
    let port = options.port.or(state.port()).unwrap_or(phone::DEFAULT_PORT);
    let phone = match reach(&mut pairing, port) {
        // A pairing the iPhone has forgotten, as when it's been reset, or
        // the pairing program's from before that.
        Err(phone::Problem::NotPaired) if !paired_now => {
            let _ = fs::remove_file(state.pairing());
            terminal::say("This iPhone doesn't know the pairing any more, so it pairs anew.");
            pairing = pairing::pair(state, options.address.as_deref())?;
            paired_now = true;
            vpn_on()?;
            reach(&mut pairing, port)
        }
        reached => reached,
    }
    .map_err(|problem| explain(problem, state, port, paired_now))?;
    state.set_port(port);
    let ios = phone
        .ios
        .as_deref()
        .map(|ios| format!(", iOS {ios}"))
        .unwrap_or_default();
    terminal::say(format!("Connected to {}{ios}.", phone.name));
    let (phone_name, udid) = (phone.name.clone(), phone.udid.clone());
    // Apple's steps take minutes of typing: a new tunnel installs.
    drop(phone);
    if options.check {
        println!("\nThe pairing and LocalDevVPN work: this iPhone is ready for the install.");
        return Ok(());
    }

    terminal::step(3, steps, "The IPA");
    let kept = kept::Install::load(state);
    if options.redo {
        kept::remove(state, kept.as_ref());
        state.forget_download();
    } else if let Some(kept) = &kept
        && let Some(link) = options
            .ipa
            .as_deref()
            .filter(|link| link.starts_with("https://"))
        && kept.is_from(link, options.sha256.as_deref())
    {
        // Its link: no need to download it again.
        return go_on(state, &mut pairing, port, kept);
    }
    let mut ipa = ipa::get(state, options.ipa.as_deref(), options.sha256.as_deref())?;
    terminal::say(format!("{} {} ({}).", ipa.name, ipa.version, ipa.bundle_id));
    if let Some(kept) = kept.filter(|_| !options.redo) {
        if kept.sha256 == ipa.sha256 {
            return go_on(state, &mut pairing, port, &kept);
        }
        // Another IPA's.
        kept.remove(state);
    }
    if let Some(path) = &options.pairing_to {
        ipa.hand_offs.pairing = Some(path.clone());
    }
    agree(&mut ipa)?;
    install(options, state, &mut pairing, &ipa, &phone_name, &udid, port)
}

/// Goes on with the install the last run signed and didn't finish.
fn go_on(
    state: &State,
    pairing: &mut Pairing,
    port: u16,
    kept: &kept::Install,
) -> Result<(), Stop> {
    terminal::say(format!(
        "The last run signed {} {} and stopped before it was installed,",
        kept.name, kept.version
    ));
    terminal::say("so this one goes on from there. ./atelier-install.wasm --redo starts over.");
    finish(state, pairing, port, kept, kept.signing(state))
}

/// Says what the app takes from this program once it's installed, and,
/// for any app but Atelier, asks: it gets nothing without a yes.
fn agree(ipa: &mut ipa::Ipa) -> Result<(), Stop> {
    if let Some(why) = &ipa.refused {
        terminal::log(format!("hand-off refused: {why}"));
        terminal::say(format!(
            "Its Info.plist asks for something this program doesn't hand over: {why}."
        ));
    }
    if ipa.hand_offs.is_empty() || ipa.bundle_id == ipa::ATELIER {
        return Ok(());
    }
    let lines = ipa.hand_offs.lines();
    terminal::say(format!("Once it's installed, {} asks for", ipa.name));
    for line in &lines {
        terminal::say(format!("- {line}."));
    }
    let question = if lines.len() == 1 {
        "Hand it over? Only to an app you trust with it."
    } else {
        "Hand them over? Only to an app you trust with them."
    };
    if !terminal::confirm(question)? {
        ipa.hand_offs = HandOffs::default();
        terminal::say(format!("{} gets none of it.", ipa.name));
    }
    Ok(())
}

/// Steps 4 to 7, with the IPA in hand.
fn install(
    options: &Options,
    state: &State,
    pairing: &mut Pairing,
    ipa: &ipa::Ipa,
    phone_name: &str,
    udid: &str,
    port: u16,
) -> Result<(), Stop> {
    terminal::step(4, STEPS, "Your Apple ID");
    let takes_sign_in = ipa.hand_offs.signing.as_ref().map(|_| ipa);
    let (developer, account) = sign_in(options, state, takes_sign_in)?;

    terminal::step(5, STEPS, &format!("Signing {}", ipa.name));
    let signed = wait(apple::sign(
        state,
        developer,
        &account.email,
        ipa,
        phone_name,
        udid,
    ))?;

    // Kept with what the app takes, so that a run stopped from here on goes
    // on from here.
    let (signing, signing_problem) = match &ipa.hand_offs.signing {
        Some(_) => match signing_file(&signed, &account, state, udid) {
            Ok(file) => (Some(file), None),
            Err(why) => (None, Some(why)),
        },
        None => (None, None),
    };
    let kept = kept::Install {
        source: options.ipa.clone().unwrap_or_default(),
        sha256: ipa.sha256.clone(),
        ipa_bundle_id: ipa.bundle_id.clone(),
        bundle_id: signed.bundle_id.clone(),
        name: ipa.name.clone(),
        version: ipa.version.clone(),
        app: signed.app.clone(),
        hand_offs: ipa.hand_offs.clone(),
        signing_problem,
        signed_at: kept::now(),
    };
    if let Err(error) = kept.save(state, signing.as_deref()) {
        terminal::log(format!("keeping the install: {error}"));
    }
    finish(state, pairing, port, &kept, signing)
}

/// Steps 6 and 7, for a signed app: until they're done, a run that stops
/// leaves it for the next.
fn finish(
    state: &State,
    pairing: &mut Pairing,
    port: u16,
    kept: &kept::Install,
    signing: Option<Vec<u8>>,
) -> Result<(), Stop> {
    terminal::step(6, STEPS, "Installing it");
    let mut phone = install_app(pairing, port, &kept.app, state)?;

    terminal::step(
        7,
        STEPS,
        &format!("What {} takes from this program", kept.name),
    );
    hand_over(&mut phone, kept, signing.as_deref(), pairing, state);
    kept.remove(state);
    state.forget_download();

    terminal::notify(&format!(
        "{} is installed. Before it opens, trust your Apple ID in Settings › General › VPN & Device Management.",
        kept.name
    ));
    println!();
    println!("{} is on your Home Screen. Before it opens:", kept.name);
    println!("  1. Settings › General › VPN & Device Management › your Apple ID › Trust.");
    println!("  2. Open {}.", kept.name);
    println!("It opens for 7 days. This test build doesn't renew it: run it again");
    println!("before then.");
    Ok(())
}

/// Installs the signed app through a new tunnel. When the connection
/// breaks, as it can while rootshell isn't on screen, it tries again on
/// the user's word, through another: the app is signed already.
fn install_app(
    pairing: &mut Pairing,
    port: u16,
    app: &Path,
    state: &State,
) -> Result<phone::Phone, Stop> {
    loop {
        let mut phone =
            reach(pairing, port).map_err(|problem| explain(problem, state, port, false))?;
        let shown = AtomicU64::new(u64::MAX);
        let checked_in = awake::checked_in();
        let installed = phone.install(app, |percent| {
            awake::check_in();
            if shown.swap(percent, Ordering::Relaxed) != percent {
                terminal::bar(percent as f64 / 100.0, format!("{percent}%"));
            }
        });
        terminal::log(format!(
            "install: checked in {} times",
            awake::checked_in() - checked_in
        ));
        match installed {
            Ok(()) => {
                terminal::done("Installed.");
                return Ok(phone);
            }
            Err(report) => {
                println!();
                let report = format!("{report:?}");
                terminal::log(format!("install: {report}"));
                if !connection_broke(&report) {
                    return Err(install_problem(&report));
                }
                terminal::notify(
                    "The install's connection broke: come back to rootshell to try again.",
                );
                terminal::say("The connection to this iPhone broke, as it can while rootshell");
                terminal::say("isn't on screen. Stay in rootshell until it's installed.");
                terminal::ask("Press Return to try again, or Control-C to stop. ")?;
            }
        }
    }
}

/// Whether what stopped the install was its connection breaking, rather
/// than the iPhone refusing it.
fn connection_broke(report: &str) -> bool {
    [
        "BrokenPipe",
        "ConnectionReset",
        "ConnectionAborted",
        "UnexpectedEof",
        "the connection broke",
    ]
    .iter()
    .any(|sign| report.contains(sign))
}

/// Step 7: hands the app, as it was just installed, what it takes:
/// `signing`, the certificate and the sign-in, when it takes them.
fn hand_over(
    phone: &mut phone::Phone,
    kept: &kept::Install,
    signing: Option<&[u8]>,
    pairing: &Pairing,
    state: &State,
) {
    let (name, atelier) = (&kept.name, kept.ipa_bundle_id == ipa::ATELIER);
    if kept.hand_offs.is_empty() {
        terminal::say(format!("{name} takes nothing from this program."));
    }
    if let Some(path) = &kept.hand_offs.pairing {
        match phone.give(&kept.bundle_id, path, &pairing.bytes) {
            Ok(()) => {
                let _ = fs::remove_file(state.pairing());
                terminal::say(format!(
                    "{name} has this iPhone's pairing now, and this program's copy is deleted."
                ));
            }
            Err(error) => {
                terminal::log(format!("giving the pairing: {error:?}"));
                terminal::say(format!("Couldn't hand {name} this iPhone's pairing."));
                if atelier {
                    terminal::say("Atelier will ask to pair in its own setup.");
                }
                terminal::say(
                    "This program keeps its copy; ./atelier-install.wasm --forget deletes it.",
                );
            }
        }
    }
    // Atelier opens signed in, and signs with the same certificate rather
    // than asking Apple for another. This program keeps the key too, for its
    // next run.
    let Some(path) = &kept.hand_offs.signing else {
        if atelier {
            terminal::say(format!(
                "This build of Atelier doesn't take a sign-in from this program. {YOURSELF}"
            ));
        }
        return;
    };
    let given = match signing {
        Some(file) => phone.give(&kept.bundle_id, path, file).map_err(|error| {
            terminal::log(format!("giving the sign-in: {error:?}"));
            format!("Couldn't hand {name} your sign-in and this program's certificate.")
        }),
        None => Err(kept
            .signing_problem
            .clone()
            .unwrap_or_else(|| "The sign-in wasn't kept from the last run.".into())),
    };
    match given {
        Ok(()) if atelier => terminal::say(
            "Atelier has your sign-in and this program's certificate: it opens signed in, and signs with this certificate rather than asking Apple for another.",
        ),
        Ok(()) => terminal::say(format!(
            "{name} has your sign-in and this program's certificate."
        )),
        Err(why) if atelier => terminal::say(format!("{why} {YOURSELF}")),
        Err(why) => terminal::say(why),
    }
}

/// What the app that takes the sign-in signs in with.
struct Account {
    email: String,
    /// None when the run signed in with the token the last one kept.
    password: Option<String>,
    /// The anisette server the sign-in went through.
    server: String,
}

/// Asks for the Apple ID and signs in to Apple: with the token the last
/// sign-in kept, when there's one and it does, otherwise with the password.
/// `takes_it`, the app that takes the sign-in once it's installed, which
/// takes the password with it.
fn sign_in(
    options: &Options,
    state: &State,
    takes_it: Option<&ipa::Ipa>,
) -> Result<(DeveloperSession, Account), Stop> {
    let email = terminal::ask("Apple ID: ")?.to_lowercase();
    if email.is_empty() {
        return Err(Stop::new("Stopped: no Apple ID was given"));
    }
    // --sign-in checks the password.
    if takes_it.is_none() && !options.sign_in {
        let anisette = options.anisette.as_deref();
        if let Some((developer, server)) =
            wait(async { Ok(apple::kept_sign_in(state, &email, anisette).await) })?
        {
            terminal::say("Signed in.");
            let password = None;
            return Ok((
                developer,
                Account {
                    email,
                    password,
                    server,
                },
            ));
        }
    }
    match takes_it {
        Some(ipa) if ipa.bundle_id == ipa::ATELIER => {
            terminal::say(
                "The password goes to Apple, and to Atelier on this iPhone, which keeps it",
            );
            terminal::say("in its Keychain to sign in. Apple may ask for a code.");
        }
        Some(ipa) => terminal::say(format!(
            "The password goes to Apple, and to {} on this iPhone. Apple may ask for a code.",
            ipa.name
        )),
        None => terminal::say(
            "The password only goes to Apple, and isn't kept. Apple may ask for a code.",
        ),
    }
    let password = terminal::ask_hidden("Password: ")?;
    if password.is_empty() {
        return Err(Stop::new("Stopped: no password was given"));
    }
    terminal::say("Signing in…");
    // rootshell's connections wait for a server for as long as it takes.
    terminal::say("If nothing more shows within a minute, a server isn't answering: press");
    terminal::say("Control-C, and run this again, which tries that server last.");
    let servers = wait(async { Ok(apple::servers(state, options.anisette.as_deref()).await) })?;
    let (developer, server, kept) = wait(apple::sign_in(state, &servers, &email, &password))?;
    terminal::say("Signed in.");
    if kept {
        terminal::say(
            "This program keeps Apple's token for this sign-in, not the password, so that",
        );
        terminal::say("its next run can sign in without the password or a code.");
        terminal::say("./atelier-install.wasm --forget deletes it.");
    }
    Ok((
        developer,
        Account {
            email,
            password: Some(password),
            server,
        },
    ))
}

/// What to do when Atelier doesn't get the sign-in.
const YOURSELF: &str = "Sign in to Atelier yourself: it then makes a certificate of its own.";

/// The certificate and the sign-in, for an app that takes them
/// (certificate.rs), or why there's none to hand over.
fn signing_file(
    signed: &apple::Signed,
    account: &Account,
    state: &State,
    udid: &str,
) -> Result<Vec<u8>, String> {
    let Some(certificate) = &signed.certificate else {
        return Err("Apple doesn't list the certificate this program signed with.".into());
    };
    let Some(password) = &account.password else {
        return Err("This run signed in without the password.".into());
    };
    let anisette_state =
        certificate::anisette_state(&state.anisette(&account.server)).map_err(|error| {
            terminal::log(format!("reading the anisette state: {error}"));
            "Couldn't read the sign-in's anisette state.".to_string()
        })?;
    let sign_in = certificate::SignIn {
        apple_id: account.email.clone(),
        password: password.clone(),
        udid: udid.to_string(),
        anisette_server: account.server.clone(),
        anisette_state,
    };
    Ok(certificate.hand_off(&sign_in))
}

/// Has the user connect LocalDevVPN, which the pairing went without.
fn vpn_on() -> Result<(), Stop> {
    // The pairing ends in Settings, where the terminal doesn't show.
    terminal::notify(
        "Paired. Open LocalDevVPN and tap Connect if it isn't connected, then come back to rootshell.",
    );
    println!();
    terminal::say("Now open LocalDevVPN and tap Connect, if it isn't connected.");
    terminal::ask("Then come back here and press Return. ")?;
    Ok(())
}

/// Opens the tunnel through the pairing service at `port`. rootshell's
/// connections wait for as long as LocalDevVPN does, so this says how to
/// stop it.
fn reach(pairing: &mut Pairing, port: u16) -> Result<phone::Phone, phone::Problem> {
    terminal::say(format!(
        "Connecting to its pairing service, at port {port}."
    ));
    terminal::say("If nothing more shows within a minute, press Control-C: LocalDevVPN");
    terminal::say("isn't connected, or the pairing service is at another port.");
    phone::connect(&mut pairing.file, port)
}

/// What to do when the tunnel didn't open. `paired_now` when the pairing
/// it went on was made in this run.
fn explain(problem: phone::Problem, state: &State, port: u16, paired_now: bool) -> Stop {
    match problem {
        phone::Problem::NoVpn(why) => {
            terminal::log(format!("{}: {why}", phone::DEVICE));
            Stop::new("LocalDevVPN isn't connected")
                .next("Open LocalDevVPN and tap Connect, on Wi-Fi, then run this again.")
        }
        phone::Problem::NoService(why) => {
            terminal::log(format!("port {port}: {why}"));
            Stop::new(format!("The iPhone's pairing service didn't answer at port {port}"))
                .next("Check that Developer Mode is on, in Settings › Privacy & Security › Developer Mode.")
                .next("Then turn LocalDevVPN off and on, and run this again.")
        }
        phone::Problem::NotPaired => {
            // Kept, it would only be refused again.
            let _ = fs::remove_file(state.pairing());
            let stop = Stop::new("This iPhone refused the pairing");
            if paired_now {
                stop.next("Run this again to pair anew, and type the code as it shows.")
            } else {
                stop.next("Run this again to pair anew.")
            }
        }
        phone::Problem::Other(why) => Stop::new(format!("Couldn't connect to this iPhone ({why})"))
            .next("Turn LocalDevVPN off and on, then run this again."),
    }
}

/// What the iPhone's refusal of the install means, when it's one of those
/// a free Apple ID runs into.
fn install_problem(report: &str) -> Stop {
    // rootcause's reports start with an empty line, then a bullet.
    let reason = report
        .lines()
        .map(|line| line.trim().trim_start_matches('●').trim())
        .find(|line| !line.is_empty())
        .unwrap_or("no reason given");
    let stop = Stop::new(format!("The iPhone didn't install it: {reason}"));
    if report.contains("maximum number of installed apps") || report.contains("MaximumAppsLimit") {
        stop.next("This iPhone holds 3 apps signed with a free Apple ID at a time: delete one, then run this again.")
    } else if report.contains("DeveloperMode") || report.contains("Developer Mode") {
        stop.next("Turn Developer Mode on, in Settings › Privacy & Security › Developer Mode, then run this again.")
    } else {
        stop.next("Run this again to try the install again, or with --redo to sign it anew first.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(arguments: &[&str]) -> Result<Options, String> {
        parse(arguments.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn no_argument_is_a_whole_install() {
        assert_eq!(options(&[]).unwrap(), Options::default());
    }

    #[test]
    fn reads_its_options() {
        let read = options(&[
            "--ipa",
            "https://example.com/Atelier.ipa",
            "--sha256",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "--port",
            "49153",
            "--address",
            "192.168.1.20",
        ])
        .unwrap();
        assert_eq!(read.ipa.as_deref(), Some("https://example.com/Atelier.ipa"));
        assert_eq!(read.port, Some(49153));
        assert_eq!(read.address.as_deref(), Some("192.168.1.20"));
        assert!(read.sha256.is_some());
        assert!(options(&["--check"]).unwrap().check);
        assert!(options(&["--sign-in"]).unwrap().sign_in);
        assert!(options(&["--forget"]).unwrap().forget);
        assert!(options(&["--redo"]).unwrap().redo);
        assert!(options(&["-h"]).unwrap().help);
    }

    #[test]
    fn refuses_what_it_cant_read() {
        assert!(options(&["--ipa"]).is_err());
        assert!(options(&["--address"]).is_err());
        assert!(options(&["--sha256", "abc"]).is_err());
        assert!(options(&["--port", "0"]).is_err());
        assert!(options(&["--port", "70000"]).is_err());
        assert!(options(&["--anisette", "http://example.com"]).is_err());
        assert!(options(&["--pairing-to", "../Documents/pairingFile.plist"]).is_err());
        assert!(options(&["--verbose"]).is_err());
    }

    #[test]
    fn takes_where_an_app_reads_its_pairing() {
        let read = options(&["--pairing-to", "Documents/pairingFile.plist"]).unwrap();
        assert_eq!(
            read.pairing_to.as_deref(),
            Some("Documents/pairingFile.plist")
        );
    }

    /// From an iPhone, 12 Nov 2026, the install's upload after rootshell
    /// went to the background.
    const BROKEN: &str = "\n ● IdeviceError(Socket(Custom { kind: BrokenPipe, error: \"send: the connection broke (29)\" }))\n ╰ vendor/isideload/src/sideload/install.rs:184\n";

    #[test]
    fn tries_again_when_the_connection_broke_not_when_refused() {
        assert!(connection_broke(BROKEN));
        assert!(!connection_broke(
            "ApplicationVerificationFailed: This device has reached the maximum number of installed apps using a free developer profile"
        ));
        assert_eq!(
            install_problem(BROKEN).what,
            "The iPhone didn't install it: IdeviceError(Socket(Custom { kind: BrokenPipe, error: \"send: the connection broke (29)\" }))"
        );
    }

    #[test]
    fn names_a_free_apple_ids_limits() {
        let full = install_problem(
            "ApplicationVerificationFailed: This device has reached the maximum number of installed apps using a free developer profile",
        );
        assert!(full.next[0].contains("3 apps"));
        assert!(install_problem("something else").next[0].contains("--redo"));
    }
}
