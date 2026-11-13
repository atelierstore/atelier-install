//! Apple's side: the sign-in, through anisette servers as the app goes
//! through them (`route.rs`), and the signing, through isideload, the
//! library the app's own Apple ID signing is built on: the certificate,
//! the device, the App ID, its profile and the signature. On this
//! program's copy of isideload, whose HTTP goes through rootshell's TLS,
//! and without a runtime: what it asks, it asks right away.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use isideload::SideloadError;
use isideload::anisette::AnisetteClientInfo;
use isideload::anisette::remote_v3::RemoteV3AnisetteProvider;
use isideload::auth::apple_account::{
    AppleAccount, TwoFactorCallbackParams, TwoFactorCallbackResponse,
};
use isideload::dev::app_ids::AppIdsApi;
use isideload::dev::certificates::DevelopmentCertificate;
use isideload::dev::developer_session::DeveloperSession;
use isideload::dev::device_type::DeveloperDeviceType;
use isideload::dev::devices::DevicesApi;
use isideload::dev::teams::{DeveloperTeam, TeamsApi};
use isideload::sideload::builder::{
    ExtensionsBehavior, ExtensionsBehaviorSelection, MaxCertsBehavior, SideloaderBuilder,
    TeamSelection,
};
use isideload::util::callbacks::{ExtensionsBehaviorCallbackBox, MaxCertsCallbackBox};
use isideload::util::fs_storage::FsStorage;
use rootcause::{Report, report};

use crate::awake;
use crate::certificate::{self, Certificate};
use crate::ipa::Ipa;
use crate::kept;
use crate::route::{self, Failure, List, Route, Server};
use crate::state::{AppleIdHash, State};
use crate::terminal::{self, Stop};
use crate::token::Token;

/// Atelier's worker, which keeps the list of anisette servers current: the
/// address Atelier asks (`AnisetteListSource.defaultURL`).
const LIST_URL: &str = "https://api.atelierstore.app/anisette";
/// The name Apple lists the certificate's machine under. Not the app's
/// "Atelier": at Apple's limit, the app offers to revoke its own
/// certificates whose key it doesn't hold, and this one signed the app.
const MACHINE_NAME: &str = "Atelier Installer";
/// Apple's "too many certificates".
const TOO_MANY_CERTIFICATES: i64 = 7460;
/// The most times to ask which certificate to revoke. isideload asks Apple
/// for the certificate 4 times in all, coming here after each "too many
/// certificates", and not again after the fourth, when a revoke would be
/// for nothing.
const MOST_REVOKES: usize = 3;

/// The servers to sign in through: the user's own, the worker's list, or
/// the one built in. What earlier runs were stopped waiting for comes last,
/// as rootshell's connections never give up on a server that doesn't
/// answer, and Control-C is the way out.
pub async fn servers(state: &State, own: Option<&str>) -> List {
    if let Some(own) = own {
        return List::own(own);
    }
    let (stalled, last) = state.take_stalled();
    let list = if stalled.iter().any(|url| url == LIST_URL) {
        terminal::log("anisette list: built in, as the worker's didn't answer before");
        List::built_in()
    } else {
        state.set_trying(LIST_URL);
        let fetched = async {
            let response = isideload::http::Client::new()
                .get(LIST_URL)
                .send()
                .await
                .ok()?;
            List::parse(&response.error_for_status().ok()?.text().await.ok()?)
        }
        .await;
        state.done_trying();
        match fetched {
            Some(list) => {
                terminal::log(format!(
                    "anisette list: the worker's, {} servers",
                    list.servers.len()
                ));
                list
            }
            None => {
                terminal::log("anisette list: built in");
                List::built_in()
            }
        }
    };
    if let Some(url) = last {
        say_stopped_at(&url, Some(&list));
    }
    terminal::log(format!("stopped at before: {}", stalled.len()));
    last_of(list, &stalled)
}

/// Says what the last run was stopped waiting for: `url`, by its name in
/// `list` when it's there.
fn say_stopped_at(url: &str, list: Option<&List>) {
    let name = if url == LIST_URL {
        "Atelier's list of servers".to_string()
    } else {
        list.and_then(|list| list.servers.iter().find(|server| server.url == url))
            .map_or(url.to_string(), |server| server.name.clone())
    };
    terminal::say(format!(
        "The last run was stopped waiting for {name}, so this one tries it last."
    ));
}

/// `list` with the servers in `stalled` last, in the order they stalled.
fn last_of(mut list: List, stalled: &[String]) -> List {
    let (mut stale, fresh): (Vec<Server>, Vec<Server>) = list
        .servers
        .into_iter()
        .partition(|server| stalled.contains(&server.url));
    stale.sort_by_key(|server| stalled.iter().position(|url| *url == server.url));
    list.servers = fresh.into_iter().chain(stale).collect();
    list
}

/// Signs in to Apple's developer services: the session, Apple's token for
/// it, with the anisette server it went through, and whether that token is
/// kept for the next run (token.rs).
pub async fn sign_in(
    state: &State,
    list: &List,
    email: &str,
    password: &str,
) -> Result<(DeveloperSession, Token, bool), Stop> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut route = Route::new(list, state.last_server().as_deref());
    let mut server = route.start();
    let mut tried = 0;
    let mut last = None;
    while let Some(current) = server {
        tried += 1;
        terminal::log(format!("sign-in through {}", current.name));
        state.set_trying(&current.url);
        let result = attempt(state, list, &current, email, password, cancelled.clone()).await;
        state.done_trying();
        match result {
            Ok((developer, token)) => {
                state.set_last_server(&current.url);
                state.clear_stalled();
                let kept = keep(state, &AppleIdHash::of(email), &token);
                return Ok((developer, token, kept));
            }
            Err(report) => {
                let problem = Problem::of(&report, cancelled.load(Ordering::SeqCst));
                terminal::log(format!("sign-in failed: {problem:?}"));
                server = route.next(problem.failure());
                last = Some(problem);
            }
        }
    }
    Err(match last {
        Some(problem) => problem.explain(tried),
        None => Stop::new("There's no server to sign in through"),
    })
}

/// Keeps Apple's token for the next run, and says in the log when Apple
/// says it ends, which tells how long a kept sign-in lasts.
fn keep(state: &State, account: &AppleIdHash, token: &Token) -> bool {
    match token.save(state, account) {
        Ok(()) => {
            terminal::log(format!(
                "kept the sign-in's token, which ends in {}",
                hours_left(token)
            ));
            true
        }
        Err(error) => {
            terminal::log(format!("keeping the sign-in's token: {error}"));
            false
        }
    }
}

/// How long until Apple says `token` ends, for the log.
fn hours_left(token: &Token) -> String {
    match token.ends_at() {
        Some(end) => format!("{} hours", end.saturating_sub(kept::now()) / 3600),
        None => "a time Apple didn't say".to_string(),
    }
}

/// isideload's provider for the anisette server `url`, with its state in
/// `.atelier`: the machine Apple knows the sign-ins through it by.
fn provider(
    state: &State,
    url: &str,
    client_info: Option<(String, String)>,
) -> Result<RemoteV3AnisetteProvider, Report> {
    let client_info = client_info.map(|(client_info, user_agent)| AnisetteClientInfo {
        client_info,
        user_agent,
    });
    Ok(RemoteV3AnisetteProvider::new(
        url,
        Box::new(FsStorage::new(state.anisette(url))),
        "0".to_string(),
    )?
    .set_client_info_override(client_info))
}

async fn attempt(
    state: &State,
    list: &List,
    server: &Server,
    email: &str,
    password: &str,
    cancelled: Arc<AtomicBool>,
) -> Result<(DeveloperSession, Token), Report> {
    let provider = provider(state, &server.url, list.client_info.clone())?;
    // Apple's answered by the time it asks for a code: a run stopped at
    // the code wasn't stopped waiting for the server.
    let answered = state.clone();
    let ask = move |params: TwoFactorCallbackParams| {
        answered.done_trying();
        let cancelled = cancelled.clone();
        async move { Ok(ask_code(&params, &cancelled)) }
    };
    let mut account = AppleAccount::builder(email)
        .anisette_provider(provider)
        .login(password, ask)
        .await?;
    // As isideload's `DeveloperSession::from_account`, keeping the token.
    let token = account.get_app_token("xcode.auth").await?;
    let adsid = account
        .spd
        .as_ref()
        .and_then(|spd| spd.get("adsid"))
        .and_then(plist::Value::as_string)
        .ok_or_else(|| report!("Apple's sign-in didn't say the account's adsid"))?
        .to_string();
    let kept = Token::new(
        &server.url,
        list.client_info.clone(),
        &adsid,
        &token,
        kept::now(),
    );
    let developer = DeveloperSession::new(
        token,
        adsid,
        account.grandslam_client.clone(),
        account.anisette_generator.clone(),
    );
    Ok((developer, kept))
}

/// Signs in with the token an earlier sign-in kept for `account`, without
/// the password, through the anisette server that one went through: the
/// session, and that token. None when there's no token worth trying, or
/// Apple doesn't take it, which it then deletes. `own`: the server the
/// user asked for, if any.
pub async fn kept_sign_in(
    state: &State,
    account: &AppleIdHash,
    own: Option<&str>,
) -> Option<(DeveloperSession, Token)> {
    let token = Token::load(state, account, kept::now())?;
    if own.is_some_and(|own| own != token.server) {
        terminal::log("kept sign-in: not tried, another server was asked for");
        return None;
    }
    // What a run stopped waiting for goes last, this run's attempt with the
    // token included; servers() says it otherwise.
    let (_, last) = state.take_stalled();
    if let Some(url) = &last {
        say_stopped_at(url, None);
    }
    if let Some(why) = not_worth_trying(&token, state) {
        terminal::log(format!("kept sign-in: not tried, {why}"));
        return None;
    }
    terminal::say("Signing in with Apple's token from the last sign-in, without the password…");
    terminal::say("If nothing more shows within a minute, its server isn't answering: press");
    terminal::say("Control-C, and run this again, which signs in with the password.");
    terminal::log(format!(
        "sign-in: the kept one, through {}, which ends in {}",
        token.server,
        hours_left(&token)
    ));
    state.set_trying(&token.server);
    let result = resume(state, account, &token).await;
    state.done_trying();
    match result {
        Ok(developer) => {
            state.set_last_server(&token.server);
            state.clear_stalled();
            Some((developer, token))
        }
        Err(report) => {
            let refused = refused(&report);
            terminal::log(format!(
                "kept sign-in failed{}: {}",
                if refused { ", refused" } else { "" },
                chain(&report)
            ));
            if refused {
                Token::forget(state, account);
                terminal::say(
                    "Apple doesn't take that token any more, so this run asks for the password.",
                );
            } else {
                terminal::say("That didn't go through, so this run asks for the password.");
            }
            None
        }
    }
}

/// Why `token` isn't worth trying, if it isn't: a run was stopped waiting
/// for its server, which rootshell's connections would wait for again for
/// good, or its server's state, the machine Apple knows the token by, is
/// gone.
fn not_worth_trying(token: &Token, state: &State) -> Option<&'static str> {
    if state.stalled_at(&token.server) {
        Some("a run was stopped waiting for its server")
    } else if certificate::anisette_state(&state.anisette(&token.server)).is_err() {
        Some("its server's state is gone")
    } else {
        None
    }
}

/// The developer session for the kept `token`, once Apple has listed the
/// account's teams with it.
async fn resume(
    state: &State,
    account: &AppleIdHash,
    token: &Token,
) -> Result<DeveloperSession, Report> {
    let provider = provider(state, &token.server, token.client_info.clone())?;
    // Only its GrandSlam client and anisette are used, which don't take the
    // Apple ID: the account isn't signed in to here.
    let account = AppleAccount::builder(account.as_str())
        .anisette_provider(provider)
        .build()
        .await?;
    let mut developer = DeveloperSession::new(
        token.app_token(),
        token.adsid.clone(),
        account.grandslam_client.clone(),
        account.anisette_generator.clone(),
    );
    developer.list_teams().await?;
    Ok(developer)
}

/// Whether Apple refused a kept token, rather than not answering: its
/// developer services said no, with a code, or with HTTP's 401 or 403.
fn refused(report: &Report) -> bool {
    matches!(Problem::of(report, false), Problem::Developer(..))
        || report.iter_reports().any(|node| {
            node.downcast_current_context::<isideload::http::Error>()
                .and_then(isideload::http::Error::status)
                .is_some_and(|status| status.as_u16() == 401 || status.as_u16() == 403)
        })
}

/// Asks for the code Apple sent, or another way to get one.
fn ask_code(params: &TwoFactorCallbackParams, cancelled: &AtomicBool) -> TwoFactorCallbackResponse {
    let abort = || {
        cancelled.store(true, Ordering::SeqCst);
        TwoFactorCallbackResponse::Abort
    };
    if let Some(error) = &params.last_error {
        terminal::say(error);
    }
    // Whether Apple texts a code as it gives the trusted numbers, and says
    // so, which spares a second text with another code; the fields alone,
    // without the numbers.
    if let Some(first) = &params.first_answer {
        let said = match first.text_sent_to {
            Some(_) => "says a code was texted",
            None => "doesn't say a code was texted",
        };
        terminal::log(format!(
            "Apple's first answer {said}: {}",
            first.fields.join(", ")
        ));
    }
    let selected = params
        .selected_number_id
        .and_then(|id| params.numbers.iter().find(|number| number.id == id))
        .or(params.numbers.first());
    match (params.unknown, params.sms, selected) {
        (true, _, _) => terminal::say("Apple wants a verification code."),
        (false, true, Some(number)) => terminal::say_personal(
            format!(
                "Apple sent a code by text message to {}.",
                number.number_with_dial_code
            ),
            "Apple sent a code by text message to a trusted number.",
        ),
        (false, true, None) => terminal::say("Apple sent a code by text message."),
        (false, false, _) => terminal::say("Apple sent a code to your other Apple devices."),
    }
    let mut ways = vec!["r to send it again"];
    if !params.numbers.is_empty() {
        ways.insert(0, "s for a text message");
    }
    if params.sms || params.unknown {
        ways.insert(0, "d to send it to your Apple devices");
    }
    loop {
        let Ok(answer) =
            terminal::ask(&format!("Code ({}, or nothing to stop): ", ways.join(", ")))
        else {
            return abort();
        };
        let digits: String = answer.chars().filter(char::is_ascii_digit).collect();
        match answer.to_lowercase().as_str() {
            "" => return abort(),
            "r" => return TwoFactorCallbackResponse::ResendCode,
            "d" => return TwoFactorCallbackResponse::SendToDevices,
            "s" => match params.numbers.as_slice() {
                [] => terminal::say("This Apple ID has no phone number for codes."),
                [number] => return TwoFactorCallbackResponse::SendSms(number.id),
                numbers => {
                    for (index, number) in numbers.iter().enumerate() {
                        terminal::say_personal(
                            format!("{}. {}", index + 1, number.number_with_dial_code),
                            format!("{}. a trusted number", index + 1),
                        );
                    }
                    let picked = terminal::ask("Which number? ")
                        .ok()
                        .and_then(|typed| typed.parse::<usize>().ok())
                        .and_then(|index| numbers.get(index.checked_sub(1)?));
                    match picked {
                        Some(number) => return TwoFactorCallbackResponse::SendSms(number.id),
                        None => terminal::say("That isn't one of them."),
                    }
                }
            },
            _ if digits.len() == 6
                && answer
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ' ' || c == '-') =>
            {
                return TwoFactorCallbackResponse::SubmitCode(digits);
            }
            _ => terminal::say("Apple's code is 6 digits."),
        }
    }
}

/// What went wrong with Apple, from isideload's report.
#[derive(Debug)]
enum Problem {
    Cancelled,
    /// Apple's sign-in refused it, with Apple's code.
    SignIn(i64, String),
    /// Apple's developer services refused it, with Apple's code.
    Developer(i64, String),
    /// Anything else, such as a server that didn't answer.
    Other(String),
}

impl Problem {
    fn of(report: &Report, cancelled: bool) -> Problem {
        if cancelled {
            return Problem::Cancelled;
        }
        for node in report.iter_reports() {
            match node.downcast_current_context::<SideloadError>() {
                Some(SideloadError::AuthWithMessage(code, message)) => {
                    return Problem::SignIn(*code, message.clone());
                }
                Some(SideloadError::DeveloperError(code, message)) => {
                    return Problem::Developer(*code, message.clone());
                }
                _ => {}
            }
        }
        Problem::Other(chain(report))
    }

    fn failure(&self) -> Failure {
        match self {
            Problem::Cancelled | Problem::Developer(..) => Failure::Final,
            Problem::SignIn(code, _) => route::failure_of(*code),
            Problem::Other(_) => Failure::Server,
        }
    }

    /// What to tell the user, in the app's words.
    fn explain(self, tried: usize) -> Stop {
        match self {
            Problem::Cancelled => Stop::new("Stopped: no code was given"),
            Problem::SignIn(-20101, _) => Stop::new("Apple says the Apple ID or password is wrong"),
            Problem::SignIn(-20209, _) => Stop::new("Apple says this Apple ID is locked")
                .next("Unlock it at iforgot.apple.com, then run this again."),
            Problem::SignIn(-80045, _) => {
                Stop::new("Apple isn't letting apps like Atelier sign in right now")
                    .next("It has happened on and off since October. Try again later.")
            }
            Problem::SignIn(code, message) => {
                Stop::new(format!("Apple refused the sign-in ({code}): {message}"))
            }
            Problem::Developer(code, message) => Stop::new(format!(
                "Apple's developer services refused it ({code}): {message}"
            )),
            Problem::Other(message) => {
                terminal::log(format!("last error: {message}"));
                let servers = if tried > 1 {
                    "the servers"
                } else {
                    "the server"
                };
                Stop::new(format!("Couldn't reach {servers} Atelier signs in through"))
                    .next("Check the connection, then run this again.")
            }
        }
    }
}

/// A report's chain of contexts, without its attachments, which can hold
/// Apple's whole answer.
fn chain(report: &Report) -> String {
    report
        .iter_reports()
        .map(|node| node.format_current_context().to_string())
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(": ")
}

pub struct Signed {
    /// The signed app's folder, in the work folder, which kept.rs deletes.
    pub app: PathBuf,
    /// The bundle ID it installs as: the IPA's and the team ID.
    pub bundle_id: String,
    /// The certificate it's signed with and its key, for Atelier, when
    /// Apple lists it.
    pub certificate: Option<Certificate>,
}

/// Signs the IPA for the iPhone `udid`, after saying what it takes and
/// asking.
pub async fn sign(
    state: &State,
    developer: DeveloperSession,
    account: &AppleIdHash,
    ipa: &Ipa,
    phone_name: &str,
    udid: &str,
) -> Result<Signed, Stop> {
    // isideload asks Apple again right after a revoke, and comes back here
    // while Apple still counts the revoked one. Then this asks again which
    // to revoke, of those still listed: on an iPhone on 12 Nov 2026, Apple
    // still counted AltStore's revoked certificate after 34 seconds and
    // after more than 25 minutes, and revoking another let it make one,
    // both times. Which, if any, stays the user's to say: it may be the one
    // Atelier or their other apps are signed with.
    let revoked = Arc::new(AtomicBool::new(false));
    let max_certs: MaxCertsCallbackBox = {
        let asked = Arc::new(AtomicUsize::new(0));
        let revoked = revoked.clone();
        Box::new(move |certificates| {
            let (asked, revoked) = (asked.clone(), revoked.clone());
            Box::pin(async move {
                let asked = asked.fetch_add(1, Ordering::SeqCst);
                if asked >= MOST_REVOKES {
                    return Ok(None);
                }
                if asked > 0 {
                    terminal::log("still at Apple's limit after the revoke");
                }
                let picked = pick_certificates(&certificates, asked > 0);
                if picked.is_some() {
                    revoked.store(true, Ordering::SeqCst);
                }
                Ok(picked)
            })
        })
    };
    let mut sideloader =
        // The hash, as the Apple ID, which isideload keeps the key by
        // (certificate::ByAppleIdHash).
        SideloaderBuilder::<MaxCertsCallbackBox, ExtensionsBehaviorCallbackBox>::new(
            developer,
            account.as_str().to_string(),
        )
        .machine_name(MACHINE_NAME.to_string())
        .storage(Box::new(certificate::ByAppleIdHash::new(
            FsStorage::new(state.apple()),
            account,
        )))
        .team_selection(TeamSelection::PromptOnce(pick_team))
        .max_certs_behavior(MaxCertsBehavior::Prompt(max_certs))
        // The app's extensions on the app's own profile, as the app signs.
        .extensions_behavior(ExtensionsBehaviorSelection::Fixed(
            ExtensionsBehavior::ReuseMain,
        ))
        .build();

    let failed = |what: &str, report: Report| {
        terminal::log(format!("{what}: {}", chain(&report)));
        match Problem::of(&report, false) {
            Problem::Developer(TOO_MANY_CERTIFICATES, _) if revoked.load(Ordering::SeqCst) => {
                Stop::new("Apple still counts the certificate you revoked")
                    .next("It has gone on counting one for more than 25 minutes.")
                    .next("Run this again later, or now to revoke another.")
            }
            Problem::Developer(TOO_MANY_CERTIFICATES, _) => Stop::new(
                "This Apple ID has as many certificates as Apple allows, and none was revoked",
            )
            .next("Run this again to pick one to revoke."),
            Problem::Developer(code, message) => Stop::new(format!(
                "Apple's developer services refused it ({code}): {message}"
            )),
            Problem::SignIn(code, message) => {
                Stop::new(format!("Apple refused it ({code}): {message}"))
            }
            Problem::Cancelled | Problem::Other(_) => {
                Stop::new(format!("{what}: {}", chain(&report)))
            }
        }
    };

    let team = sideloader
        .get_team()
        .await
        .map_err(|report| failed("Couldn't get the Apple ID's team", report))?;

    let bundle_id = format!("{}.{}", ipa.bundle_id, team.team_id);
    let app_ids = sideloader
        .get_dev_session()
        .list_app_ids(&team, None::<DeveloperDeviceType>)
        .await
        .map_err(|report| failed("Couldn't list the team's App IDs", report))?;
    terminal::say_personal(
        format!("{} {} installs as {bundle_id}.", ipa.name, ipa.version),
        format!(
            "{} {} installs as {}.<team ID>.",
            ipa.name, ipa.version, ipa.bundle_id
        ),
    );
    if app_ids
        .app_ids
        .iter()
        .any(|app_id| app_id.identifier == bundle_id)
    {
        terminal::say("Apple has its App ID already, so it takes none of this week's.");
    } else {
        match app_ids.available_quantity {
            Some(left) if left <= 0 => {
                return Err(Stop::new("This Apple ID has no App ID left this week")
                    .next("A free Apple ID gets 10 a week, each back 7 days after it was added.")
                    .next("Run this again then, or sign in with another Apple ID."));
            }
            Some(left) => terminal::say(format!(
                "Apple adds an App ID for it: 1 of the {left} this Apple ID has left this week."
            )),
            None => terminal::say("Apple adds an App ID for it."),
        }
    }
    terminal::say(
        "With a free Apple ID, it opens for 7 days, and an iPhone holds 3 such apps at a time.",
    );
    if !terminal::confirm("Sign and install it?")? {
        return Err(Stop::new("Stopped: nothing was signed or installed"));
    }

    sideloader
        .get_dev_session()
        .ensure_device_registered(&team, phone_name, udid, None::<DeveloperDeviceType>)
        .await
        .map_err(|report| failed("Couldn't add this iPhone to the Apple ID's devices", report))?;

    // On an iPhone, the signing took moments with rootshell on screen,
    // and minutes when it went to another app meanwhile, which also lost
    // its connection to the iPhone. On screen too, it stopped at 50% until
    // a key was pressed, before it checked in as it hashes (awake.rs).
    terminal::say("Stay in rootshell until it's installed: in another app, it slows down,");
    terminal::say("and can lose its connection to this iPhone.");
    let progress = |fraction: f32| async move {
        terminal::bar(
            f64::from(fraction),
            format!("{}%", (fraction * 100.0) as u32),
        );
    };
    let checked_in = awake::checked_in();
    let (app, _) = sideloader
        .sign_app(
            ipa.path.clone(),
            Some(team.clone()),
            false,
            Some(progress),
            None,
            Some(udid),
        )
        .await
        .map_err(|report| {
            println!();
            failed("Couldn't sign it", report)
        })?;
    terminal::done("Signed.");
    terminal::log(format!(
        "signing: checked in {} times",
        awake::checked_in() - checked_in
    ));

    // The certificate it signed with, which Atelier then signs with too.
    let storage = FsStorage::new(state.apple());
    let certificate = certificate::kept(
        &storage,
        sideloader.get_dev_session(),
        &team,
        account,
        MACHINE_NAME,
    )
    .await
    .unwrap_or_else(|report| {
        terminal::log(format!("finding the certificate: {}", chain(&report)));
        None
    });
    if certificate.is_none() {
        terminal::log("the certificate it signed with isn't listed");
    }
    Ok(Signed {
        app,
        bundle_id,
        certificate,
    })
}

/// Which of the Apple ID's teams to sign with, when it's in several.
// isideload's `TeamSelection` takes this very type.
#[allow(clippy::ptr_arg)]
fn pick_team(teams: &Vec<DeveloperTeam>) -> Option<String> {
    terminal::say("This Apple ID is in more than one team:");
    for (index, team) in teams.iter().enumerate() {
        terminal::say_personal(
            format!(
                "{}. {} ({})",
                index + 1,
                team.name.as_deref().unwrap_or("No name"),
                team.team_id
            ),
            format!("{}. a team", index + 1),
        );
    }
    let typed = terminal::ask("Which one signs Atelier? ").ok()?;
    let index = typed.parse::<usize>().ok()?.checked_sub(1)?;
    teams.get(index).map(|team| team.team_id.clone())
}

/// A certificate's machine as the log has it: a Mac's name can have its
/// owner's in it, as Xcode names them, so only Atelier's and this
/// program's are kept.
fn logged_machine(made_on: &str) -> &str {
    match made_on {
        MACHINE_NAME | "Atelier" => made_on,
        _ => "another machine",
    }
}

/// At Apple's limit: which certificate to revoke, by its serial number,
/// or None to stop. Never without asking. `again` when Apple still counts
/// one revoked just before.
fn pick_certificates(certificates: &[DevelopmentCertificate], again: bool) -> Option<Vec<String>> {
    println!();
    if again {
        terminal::say("Apple still counts the certificate you revoked, though it no longer");
        terminal::say("lists it. Waiting hasn't helped with that before, even for 25 minutes,");
        terminal::say("but revoking another has, though apps signed with that one stop");
        terminal::say("opening too, until they're signed again.");
    } else {
        terminal::say("This Apple ID has as many certificates as Apple allows.");
        terminal::say("Apple only makes another once one is revoked, and apps signed with");
        terminal::say("a revoked certificate stop opening until they're signed again.");
    }
    let listed: Vec<&DevelopmentCertificate> = certificates
        .iter()
        .filter(|certificate| certificate.serial_number.is_some())
        .collect();
    if listed.is_empty() {
        terminal::say("Apple lists none to revoke: it's still counting one revoked before.");
        return None;
    }
    for (index, certificate) in listed.iter().enumerate() {
        let made_on = certificate
            .machine_name
            .as_deref()
            .unwrap_or("an unnamed machine");
        let expires = certificate
            .expiration_date
            .map(|date| {
                format!(
                    ", ends {}",
                    date.to_xml_format().chars().take(10).collect::<String>()
                )
            })
            .unwrap_or_default();
        // Its name has the Apple ID's owner's name in it, which stays out
        // of the log, as the log is what people share.
        println!(
            "  {}. {} (made on {made_on}{expires})",
            index + 1,
            certificate.name.as_deref().unwrap_or("Certificate")
        );
        terminal::log(format!(
            "  {}. made on {}{expires}",
            index + 1,
            logged_machine(made_on)
        ));
    }
    let typed = terminal::ask("Number to revoke, or nothing to stop: ").ok()?;
    let certificate = listed.get(typed.parse::<usize>().ok()?.checked_sub(1)?)?;
    let made_on = certificate
        .machine_name
        .as_deref()
        .unwrap_or("an unnamed machine");
    if !terminal::confirm(&format!("Revoke the certificate made on {made_on}?")).ok()? {
        return None;
    }
    terminal::log(format!(
        "revoking the certificate made on {}",
        logged_machine(made_on)
    ));
    certificate.serial_number.clone().map(|serial| vec![serial])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(urls: &[&str]) -> List {
        List {
            tries: 4,
            client_info: None,
            servers: urls
                .iter()
                .map(|url| Server {
                    url: url.to_string(),
                    name: url.to_string(),
                    kind: None,
                })
                .collect(),
        }
    }

    fn urls(list: List) -> Vec<String> {
        list.servers.into_iter().map(|server| server.url).collect()
    }

    #[test]
    fn the_log_names_only_our_own_machines() {
        assert_eq!(logged_machine(MACHINE_NAME), MACHINE_NAME);
        assert_eq!(logged_machine("Atelier"), "Atelier");
        assert_eq!(logged_machine("Someone's MacBook Pro"), "another machine");
    }

    #[test]
    fn servers_runs_were_stopped_at_go_last_in_the_order_they_stalled() {
        assert_eq!(
            urls(last_of(list(&["a", "b", "c", "d"]), &[])),
            ["a", "b", "c", "d"]
        );
        let stalled = ["c".to_string(), "a".to_string(), LIST_URL.to_string()];
        assert_eq!(
            urls(last_of(list(&["a", "b", "c", "d"]), &stalled)),
            ["b", "d", "c", "a"]
        );
    }

    /// Each of these stops before anything goes out, and leaves the token
    /// for a run that can try it.
    #[test]
    fn a_kept_sign_in_isnt_tried_where_it_cant_go_through() {
        use isideload::anisette::remote_v3::ANISETTE_STATE_KEY;

        let folder = crate::testing::scratch("kept-sign-in");
        let state = State::at(folder.clone());
        let (account, server) = (AppleIdHash::of("a@example.com"), "https://ani.example");
        let tried = |own: Option<&str>| {
            crate::blocking::run(kept_sign_in(&state, &account, own))
                .ok()
                .flatten()
                .is_some()
        };
        // None kept.
        assert!(!tried(None));

        Token {
            server: server.into(),
            client_info: None,
            adsid: "000123-45-6789".into(),
            token: "a token".into(),
            duration: 3600,
            expiry: 0,
            saved_at: kept::now(),
        }
        .save(&state, &account)
        .unwrap();
        // Its server's state, the machine Apple knows it by, is gone.
        assert!(!tried(None));

        let anisette = state.anisette(server);
        std::fs::create_dir_all(&anisette).unwrap();
        std::fs::write(anisette.join(ANISETTE_STATE_KEY), b"state").unwrap();
        // Another server was asked for.
        assert!(!tried(Some("https://other.example")));
        // The last run was stopped waiting for it: it stays among those
        // the sign-in with the password tries last.
        state.set_trying(server);
        assert!(!tried(None));
        assert!(state.stalled_at(server));
        assert_eq!(state.take_stalled(), (vec![server.to_string()], None));

        assert!(Token::load(&state, &account, kept::now()).is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
