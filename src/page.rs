//! The installer's own page (`--page`), which the Install Atelier Shortcut
//! shows over rootshell. It asks what the terminal would, in the website's
//! look, as the mockup Alex approved on 13 Nov 2026 draws it, and shows how
//! the install goes. The terminal says it all too, and the log as ever.
//!
//! The program serves it at 127.0.0.1, on the port the Shortcut picked, one
//! connection at a time (`door.rs`): while it waits for an answer, and as
//! it works, between the pieces of the download, the signing and the
//! install. The page asks for the state about every second, and shows it.
//!
//! rootshell's listener is on every address the iPhone has, and anything on
//! the iPhone can reach 127.0.0.1. So the page's address carries a key the
//! Shortcut made, after its `#`, which a browser never sends: the page
//! reads it, and sends it with every question it asks the program. Only the
//! page's own files go without it. A page elsewhere can't have the browser
//! ask under this page's name (the Host check), nor send a header of its
//! own here without asking first, which nothing here allows.
//!
//! What's typed on the page, the Apple ID, its password and Apple's code,
//! goes only to the step that asked for it, and never into the log.
//!
//! While you pair, the page is closed: the Shortcut's web view goes when you
//! leave rootshell for Settings, and rootshell keeps a program going behind
//! another app for about 25 seconds only, without Location Diary Mode (its
//! `BackgroundExecutionCoordinator`). So the Shortcut asks how pairing goes,
//! with the key too, at `/pairing/<what it knows>`, and the program holds
//! the question until that changes, or for [`HOLD`] at most: the moment
//! Settings asks for the code, the Shortcut shows it in a notification;
//! once paired, it opens rootshell and the page again; and on no news, it
//! opens rootshell for a moment, which gives it another 25 seconds.

mod http;

use std::cell::RefCell;
use std::fmt;
use std::io::Write;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::door::{Door, Visitor};
use crate::terminal::{self, Stop};
use http::{Nothing, Request};

const INDEX: &[u8] = include_bytes!("page/index.html");
const STYLE: &[u8] = include_bytes!("page/page.css");
const SCRIPT: &[u8] = include_bytes!("page/page.js");
const ICON: &[u8] = include_bytes!("page/icon.png");
/// Bricolage Grotesque, the website's, for the headings (`page/OFL.txt`).
const FONT: &[u8] = include_bytes!("page/display.woff2");

const TEXT: &str = "text/plain; charset=utf-8";
const JSON: &str = "application/json";

/// How long the program works between two looks at its port, at most.
const TICK: Duration = Duration::from_millis(400);
/// How many connections it answers in a row, before it works again.
const IN_A_ROW: usize = 8;
/// How long it goes on answering, once it's done, for the page to show the
/// last view: the page may have been closed.
const LAST: Duration = Duration::from_secs(30);
/// How long the Shortcut's question about pairing waits for news, at most:
/// well inside the 25 seconds or so that rootshell keeps going behind
/// Settings, so the Shortcut can bring it to the front before it pauses.
const HOLD: Duration = Duration::from_secs(12);
/// What Pair says once you've been to Settings, if the page comes back
/// before it's paired.
const NOT_YET: &str = "Not paired yet. Tap Done to go back to Settings and finish pairing.";

/// The page's address, as the Shortcut makes it: `http://127.0.0.1:PORT/#KEY`.
#[derive(Clone, PartialEq, Eq)]
pub struct Address {
    pub port: u16,
    key: String,
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address {{ port: {}, key: … }}", self.port)
    }
}

impl Address {
    pub fn parse(link: &str) -> Result<Address, String> {
        const WHAT: &str = "--page takes the page's address, http://127.0.0.1:PORT/#KEY, with a port from 1024 up and a key of 16 to 128 letters and digits";
        let rest = link.strip_prefix("http://127.0.0.1:").ok_or(WHAT)?;
        let (port, key) = rest.split_once("/#").ok_or(WHAT)?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|port| *port >= 1024)
            .ok_or(WHAT)?;
        let fits = (16..=128).contains(&key.len())
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !fits {
            return Err(WHAT.into());
        }
        Ok(Address {
            port,
            key: key.to_string(),
        })
    }
}

/// What the page shows. Each screen's wording is the page's
/// (`page/index.html`): the program gives what changes.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "screen", rename_all = "kebab-case")]
pub enum View {
    /// A step under way, with how far along it is when that's known.
    Busy { title: String, bar: Option<Bar> },
    /// The code to type in Settings, shown before going there, and why the
    /// last try didn't pair.
    Pair {
        code: String,
        problem: Option<String>,
    },
    /// The Apple ID, and its password unless Apple's token may do.
    SignIn {
        password: bool,
        problem: Option<String>,
    },
    /// Apple's code: sent to the Apple ID's devices, or texted, to the
    /// number `texted` when Apple says which.
    Code {
        texted: Option<String>,
        problem: Option<String>,
        /// The other ways to get one.
        by_text: bool,
        to_devices: bool,
    },
    /// A question the page has no screen of its own for: `options` to pick
    /// one of, if any, then yes or no.
    Question {
        title: String,
        text: Vec<String>,
        options: Vec<String>,
        yes: String,
        no: Option<String>,
    },
    /// What signing and installing it takes, before it's done.
    Review {
        release: String,
        size: String,
        app_id: String,
    },
    /// Signing, then installing.
    Installing { signed: bool, bar: Bar },
    /// Installed: `release`, its name and version, and `handed` when
    /// Atelier has the pairing and the sign-in.
    Done { release: String, handed: bool },
    /// Why the program stopped, and what to do.
    Stopped { what: String, next: Vec<String> },
    /// Stopped from the page, by its Stop the installer.
    Ended,
}

impl View {
    /// Which of the page's steps it's in: Pair, Sign in, Install, then all
    /// three done. None for a screen any step can have.
    fn stage(&self) -> Option<u8> {
        match self {
            View::Pair { .. } => Some(0),
            View::SignIn { .. } | View::Code { .. } => Some(1),
            View::Review { .. } | View::Installing { .. } => Some(2),
            View::Done { .. } => Some(3),
            View::Busy { .. } | View::Question { .. } | View::Stopped { .. } | View::Ended => None,
        }
    }

    fn bar_mut(&mut self) -> Option<&mut Bar> {
        match self {
            View::Busy { bar: Some(bar), .. } | View::Installing { bar, .. } => Some(bar),
            _ => None,
        }
    }
}

/// A progress bar: how far along, and what it's of.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bar {
    percent: u8,
    text: String,
}

impl Bar {
    pub fn new(fraction: f64, text: impl Into<String>) -> Bar {
        Bar {
            // NaN goes to 0.
            percent: (fraction.clamp(0.0, 1.0) * 100.0) as u8,
            text: text.into(),
        }
    }
}

/// How pairing goes, as the Shortcut asks while you're in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pairing {
    /// Not pairing: before Pair, or after the program stopped.
    None,
    /// On Pair, before Settings starts.
    Waiting,
    /// Settings has asked for the code.
    Asked,
    /// Paired, now or on an earlier run.
    Paired,
}

impl Pairing {
    fn name(self) -> &'static str {
        match self {
            Pairing::None => "none",
            Pairing::Waiting => "waiting",
            Pairing::Asked => "asked",
            Pairing::Paired => "paired",
        }
    }

    fn named(name: &str) -> Option<Pairing> {
        [
            Pairing::None,
            Pairing::Waiting,
            Pairing::Asked,
            Pairing::Paired,
        ]
        .into_iter()
        .find(|pairing| pairing.name() == name)
    }

    /// Whether there's news to wait for.
    fn going(self) -> bool {
        matches!(self, Pairing::Waiting | Pairing::Asked)
    }
}

/// The Shortcut's question about pairing, held until there's news.
struct Held {
    visitor: Visitor,
    known: Pairing,
    until: Instant,
}

/// What the page sent back. Its fields can hold a password, so it has no
/// Debug, and goes nowhere but the step that asked.
pub struct Answer(Map<String, Value>);

impl Answer {
    /// A field's text, empty when it wasn't sent.
    pub fn text(&self, field: &str) -> &str {
        self.0
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// Whether the page said yes, as its main button does.
    pub fn yes(&self) -> bool {
        self.0.get("yes").and_then(Value::as_bool).unwrap_or(false)
    }

    /// Which option was picked, from 0.
    pub fn pick(&self) -> Option<usize> {
        self.0
            .get("pick")
            .and_then(Value::as_u64)
            .and_then(|pick| usize::try_from(pick).ok())
    }
}

/// Ends the program as Control-C does, once the page has its answer: what
/// each step made is kept for the next run, which picks up there.
#[cfg(not(test))]
fn end_from_page() -> ! {
    terminal::log("the page: stopped from the page");
    println!();
    println!("Stopped from the page. The next run picks up where this one left off.");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    std::process::exit(0)
}

/// The tests stay in their process: they look at `ending`.
#[cfg(test)]
fn end_from_page() {}

thread_local! {
    static PAGE: RefCell<Option<Page>> = const { RefCell::new(None) };
}

fn with<T>(f: impl FnOnce(&mut Page) -> T) -> Option<T> {
    PAGE.with_borrow_mut(|page| page.as_mut().map(f))
}

/// Starts serving the page at `address`.
pub fn start(address: &Address) -> Result<(), Stop> {
    let page = Page::open(address).map_err(|error| {
        terminal::log(format!("the page's port: {error}"));
        Stop::new(format!("Couldn't serve the page at port {}", address.port))
            .next("Run the Shortcut again: it picks another port each time.")
    })?;
    // The key only in the terminal, never in the log.
    println!(
        "  The page is at http://127.0.0.1:{}/#{}",
        address.port, address.key
    );
    terminal::log(format!(
        "the page: port {}, {} s in",
        address.port,
        terminal::seconds()
    ));
    PAGE.set(Some(page));
    Ok(())
}

/// Whether the program has a page to show.
pub fn on() -> bool {
    PAGE.with_borrow(Option::is_some)
}

/// Waits for Safari to have the page, answering it meanwhile: with the page,
/// nothing happens before it shows. The Shortcut opens Safari a moment after
/// rootshell, and only the page asks for the state with the key.
pub fn wait_for_visit() -> Result<(), Stop> {
    if with(|page| page.visited).unwrap_or(true) {
        return Ok(());
    }
    terminal::say("Waiting for Safari to open the page.");
    with(Page::wait_for_visit).unwrap_or(Ok(()))
}

/// Shows `view`, and answers whoever is waiting.
pub fn show(view: View) {
    with(|page| {
        page.set(view, false);
        page.serve_waiting();
    });
}

/// Shows `view`, and waits for the page's answer to it, answering the page
/// meanwhile.
pub fn ask(view: View) -> Result<Answer, Stop> {
    with(|page| page.ask(view)).unwrap_or_else(|| {
        Err(Stop::new(
            "There's no page to ask on. That's a bug in this program",
        ))
    })
}

/// Moves the view's bar, as a step goes, and answers the page now and then.
pub fn bar(fraction: f64, text: impl Into<String>) {
    with(|page| {
        let bar = Bar::new(fraction, text);
        if let Some(shown) = page.view.bar_mut()
            && *shown != bar
        {
            *shown = bar;
            page.seq += 1;
        }
    });
    tick();
}

/// Answers whoever is waiting, if the program hasn't for a moment: as it
/// works.
pub fn tick() {
    with(|page| {
        if page.served.elapsed() >= TICK {
            page.serve_waiting();
        }
    });
}

/// Once the iPhone is paired, now or before: the stepper shows pairing
/// done from the next view on.
pub fn paired() {
    with(|page| {
        page.stage = page.stage.max(1);
        page.set_pairing(Pairing::Paired);
    });
}

/// Settings has started pairing and asks for the code: the Shortcut, if
/// it's waiting, shows it now.
pub fn pairing_asked() {
    with(|page| page.set_pairing(Pairing::Asked));
}

/// Shows why the program stopped.
pub fn stopped(stop: &Stop) {
    show(stopped_view(stop));
}

/// The terminal's words for a stop, as the page says them: under its title,
/// "The install stopped", and with the Shortcut to run it again, which
/// takes no options.
fn stopped_view(stop: &Stop) -> View {
    let what = stop.what.trim_end_matches('.');
    let what = match what.strip_prefix("Stopped: ") {
        Some(rest) => {
            let mut letters = rest.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        }
        None => what.to_string(),
    };
    let next = stop
        .next
        .iter()
        .map(|line| {
            let line = match line.split_once(", or with --") {
                Some((before, _)) => format!("{before}."),
                None => line.clone(),
            };
            line.replace("Run this again", "Run the Shortcut again")
                .replace("run this again", "run the Shortcut again")
        })
        .collect();
    View::Stopped {
        what: format!("{what}."),
        next,
    }
}

/// Before the program ends: answers the page until it has the last view,
/// for [`LAST`] at most, as it may have been closed.
pub fn end() {
    with(|page| {
        let until = Instant::now() + LAST;
        while page.fetched < page.seq && Instant::now() < until && !page.failed {
            page.serve_waiting();
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

struct Page {
    door: Door,
    port: u16,
    key: String,
    /// The view's number, which changes with it, so the page knows to show
    /// it anew.
    seq: u64,
    /// The step the page's stepper shows.
    stage: u8,
    view: View,
    /// Whether the view waits for an answer, and that answer once it came.
    asking: bool,
    answer: Option<Answer>,
    /// The latest view the page has fetched.
    fetched: u64,
    /// When the program last answered whoever was waiting.
    served: Instant,
    /// Whether the port has stopped working, which the log says once.
    failed: bool,
    /// Whether the page has asked for the state with the key yet.
    visited: bool,
    /// Whether the page asked the program to stop, which it does once it
    /// has answered.
    ending: bool,
    /// How pairing goes, and the Shortcut's questions about it.
    pairing: Pairing,
    held: Vec<Held>,
    /// Whether the Shortcut has waited on pairing since Pair showed: you
    /// went to Settings.
    in_settings: bool,
}

impl Page {
    fn open(address: &Address) -> std::io::Result<Page> {
        Ok(Page {
            door: Door::open(address.port)?,
            port: address.port,
            key: address.key.clone(),
            seq: 1,
            stage: 0,
            view: View::Busy {
                title: "Getting ready".into(),
                bar: None,
            },
            asking: false,
            answer: None,
            fetched: 0,
            served: Instant::now(),
            failed: false,
            visited: false,
            ending: false,
            pairing: Pairing::None,
            held: Vec::new(),
            in_settings: false,
        })
    }

    fn set(&mut self, view: View, asking: bool) {
        if let Some(stage) = view.stage() {
            self.stage = stage;
        }
        match &view {
            // Shown anew, for a first try or another.
            View::Pair { .. } => self.set_pairing(Pairing::Waiting),
            View::Stopped { .. } | View::Ended if self.pairing.going() => {
                self.set_pairing(Pairing::None)
            }
            _ => {}
        }
        self.view = view;
        self.asking = asking;
        self.answer = None;
        self.seq += 1;
    }

    fn ask(&mut self, view: View) -> Result<Answer, Stop> {
        self.set(view, true);
        loop {
            if let Some(answer) = self.answer.take() {
                self.asking = false;
                return Ok(answer);
            }
            match self.door.wait() {
                Ok(visitor) => self.serve(visitor),
                Err(error) => {
                    terminal::log(format!("the page's port: {error}"));
                    self.failed = true;
                    return Err(Stop::new("The page's port stopped working")
                        .next("Run the Shortcut again."));
                }
            }
        }
    }

    fn wait_for_visit(&mut self) -> Result<(), Stop> {
        while !self.visited {
            match self.door.wait() {
                Ok(visitor) => self.serve(visitor),
                Err(error) => {
                    terminal::log(format!("the page's port: {error}"));
                    self.failed = true;
                    return Err(Stop::new("The page's port stopped working")
                        .next("Run the Shortcut again."));
                }
            }
        }
        Ok(())
    }

    fn serve_waiting(&mut self) {
        self.answer_held();
        if self.failed {
            return;
        }
        for _ in 0..IN_A_ROW {
            match self.door.next() {
                Ok(Some(visitor)) => self.serve(visitor),
                Ok(None) => break,
                Err(error) => {
                    terminal::log(format!("the page's port: {error}"));
                    self.failed = true;
                    break;
                }
            }
        }
        self.served = Instant::now();
    }

    fn serve(&mut self, mut visitor: Visitor) {
        // It had its time (door::FIRST_WAIT), as a connection Safari
        // opens ahead of a request can take.
        if visitor.said_nothing() {
            terminal::log("the page: a connection said nothing in time");
            return;
        }
        let reply = match http::read(&mut visitor) {
            Ok(request) => match self.held_until_news(&request) {
                Some(known) => {
                    self.held.push(Held {
                        visitor,
                        known,
                        until: Instant::now() + HOLD,
                    });
                    return;
                }
                None => self.reply(&request),
            },
            Err(Nothing::Silent | Nothing::CutShort) => return,
            Err(Nothing::NotHttp | Nothing::TooLong) => http::answer(
                "400 Bad Request",
                TEXT,
                b"This is the installer's page, over plain HTTP.\n",
            ),
        };
        let _ = visitor.write_all(&reply);
        drop(visitor);
        if self.ending {
            end_from_page();
        }
    }

    /// Whether `request` came under the page's own name.
    fn here(&self, request: &Request) -> bool {
        let port = self.port;
        let here = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        request
            .header("host")
            .is_some_and(|host| here.iter().any(|name| name == host))
    }

    fn keyed(&self, request: &Request) -> bool {
        request
            .header("x-atelier-key")
            .is_some_and(|key| same(key, &self.key))
    }

    /// What the Shortcut knows of pairing, when it asks with the key and
    /// that's still so: its question waits for news.
    fn held_until_news(&mut self, request: &Request) -> Option<Pairing> {
        if request.method != "GET" || !self.here(request) || !self.keyed(request) {
            return None;
        }
        let known = Pairing::named(request.path.strip_prefix("/pairing/")?)?;
        if known.going() {
            self.in_settings = true;
        }
        (known == self.pairing && known.going()).then_some(known)
    }

    fn reply(&mut self, request: &Request) -> Vec<u8> {
        let port = self.port;
        if !self.here(request) {
            let only =
                format!("This page only opens at http://127.0.0.1:{port}/ on this iPhone.\n");
            return http::answer("403 Forbidden", TEXT, only.as_bytes());
        }
        let keyed = self.keyed(request);
        let file = |kind: &str, body: &[u8]| http::answer("200 OK", kind, body);
        let path = request.path.as_str();
        if path == "/pairing" || path.starts_with("/pairing/") {
            if request.method != "GET" || !keyed {
                return http::answer("403 Forbidden", TEXT, b"Only the Shortcut asks this.\n");
            }
            let known = path.strip_prefix("/pairing/").and_then(Pairing::named);
            return file(JSON, &self.pairing_news(known));
        }
        match (request.method.as_str(), path) {
            ("GET", "/") => file("text/html; charset=utf-8", INDEX),
            ("GET", "/page.css") => file("text/css; charset=utf-8", STYLE),
            ("GET", "/page.js") => file("text/javascript; charset=utf-8", SCRIPT),
            ("GET", "/icon.png") => file("image/png", ICON),
            ("GET", "/display.woff2") => file("font/woff2", FONT),
            ("GET", "/state") | ("POST", "/answer") | ("POST", "/stop") if !keyed => {
                http::answer("403 Forbidden", TEXT, b"Only the page asks this.\n")
            }
            ("GET", "/state") => {
                if !self.visited {
                    self.visited = true;
                    terminal::log(format!(
                        "the page: first visit, {} s in",
                        terminal::seconds()
                    ));
                }
                self.not_yet();
                self.fetched = self.seq;
                file(JSON, &self.state())
            }
            ("POST", "/answer")
                if !request
                    .header("content-type")
                    .is_some_and(|kind| kind.starts_with(JSON)) =>
            {
                http::answer("415 Unsupported Media Type", TEXT, b"Answers are JSON.\n")
            }
            ("POST", "/answer") => {
                let taken = self.take(&request.body);
                file(JSON, json!({ "taken": taken }).to_string().as_bytes())
            }
            ("POST", "/stop") => {
                // Once it's done or stopped, there's nothing left to stop.
                if !matches!(
                    self.view,
                    View::Done { .. } | View::Stopped { .. } | View::Ended
                ) {
                    self.set(View::Ended, false);
                    self.ending = true;
                }
                file(JSON, &self.state())
            }
            _ => http::answer("404 Not Found", TEXT, b"Nothing here.\n"),
        }
    }

    /// The state, as the page reads it.
    fn state(&self) -> Vec<u8> {
        json!({
            "seq": self.seq,
            "stage": self.stage,
            "asking": self.asking,
            "view": self.view,
        })
        .to_string()
        .into_bytes()
    }

    /// How pairing goes, for the Shortcut: whether it changed from what the
    /// Shortcut `known`, what the Shortcut does next, and, once Settings
    /// asks for it, the code and the notification's words. Next is
    /// `notify` (show the notice, then ask again), `refresh` (no news:
    /// bring rootshell to the front for a moment, then ask again), `ask`
    /// (ask again) or `return` (bring back rootshell and the page).
    fn pairing_news(&self, known: Option<Pairing>) -> Vec<u8> {
        let changed = known != Some(self.pairing);
        let next = match self.pairing {
            Pairing::None | Pairing::Paired => "return",
            _ if !changed => "refresh",
            Pairing::Asked => "notify",
            Pairing::Waiting => "ask",
        };
        let mut news = json!({
            "pairing": self.pairing.name(),
            "changed": changed,
            "next": next,
        });
        if self.pairing == Pairing::Asked
            && let View::Pair { code, .. } = &self.view
        {
            news["code"] = json!(code);
            news["notice"] = json!(format!("Code for Settings: {}", spaced(code)));
        }
        news.to_string().into_bytes()
    }

    fn set_pairing(&mut self, pairing: Pairing) {
        if pairing == Pairing::Paired {
            self.in_settings = false;
        }
        if pairing != self.pairing {
            self.pairing = pairing;
            self.answer_held();
        }
    }

    /// Answers the Shortcut's questions that have news, or have waited
    /// long enough.
    fn answer_held(&mut self) {
        let now = Instant::now();
        let (due, kept): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| held.known != self.pairing || now >= held.until);
        self.held = kept;
        for mut held in due {
            let reply = http::answer("200 OK", JSON, &self.pairing_news(Some(held.known)));
            let _ = held.visitor.write_all(&reply);
        }
    }

    /// Pair, when the page comes back before it's paired, after you went
    /// to Settings: says to go back there.
    fn not_yet(&mut self) {
        if !self.in_settings || !self.pairing.going() {
            return;
        }
        if let View::Pair { problem, .. } = &mut self.view
            && problem.is_none()
        {
            *problem = Some(NOT_YET.into());
            self.seq += 1;
        }
    }

    /// Takes an answer to the view the page is showing, the first one only.
    fn take(&mut self, body: &[u8]) -> bool {
        let Ok(Value::Object(mut sent)) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        let to_this = sent.get("seq").and_then(Value::as_u64) == Some(self.seq);
        match sent.remove("answer") {
            Some(Value::Object(answer)) if to_this && self.asking && self.answer.is_none() => {
                self.answer = Some(Answer(answer));
                true
            }
            _ => false,
        }
    }
}

/// A 6-digit code as people read one: "123 456".
pub fn spaced(code: &str) -> String {
    match code.len() {
        6 if code.is_ascii() => format!("{} {}", &code[..3], &code[3..]),
        _ => code.to_string(),
    }
}

/// Whether two keys are the same, in as long whatever differs.
fn same(given: &str, key: &str) -> bool {
    given.len() == key.len()
        && given
            .bytes()
            .zip(key.bytes())
            .fold(0, |differs, (a, b)| differs | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::door::tests::{browser, opened, read_all};

    const KEY: &str = "123456789123456789123456789";

    fn page() -> Page {
        let (door, port) = opened();
        Page {
            door,
            port,
            key: KEY.into(),
            seq: 1,
            stage: 0,
            view: View::Busy {
                title: "Getting ready".into(),
                bar: None,
            },
            asking: false,
            answer: None,
            fetched: 0,
            served: Instant::now(),
            failed: false,
            visited: false,
            ending: false,
            pairing: Pairing::None,
            held: Vec::new(),
            in_settings: false,
        }
    }

    /// The status line and the body of an answer.
    fn read_answer(stream: crate::net::Stream) -> (String, String) {
        let answer = String::from_utf8(read_all(stream)).unwrap();
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        (head.lines().next().unwrap().to_string(), body.to_string())
    }

    /// Sends `request` as a browser would, has the page answer whoever is
    /// waiting, and returns the answer.
    fn exchange(page: &mut Page, request: &str) -> (String, String) {
        let sent = browser(page.port, request.as_bytes());
        page.serve_waiting();
        read_answer(sent)
    }

    fn get(page: &mut Page, path: &str, key: Option<&str>) -> (String, String) {
        let port = page.port;
        let key = key.map_or(String::new(), |key| format!("X-Atelier-Key: {key}\r\n"));
        exchange(
            page,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{key}\r\n"),
        )
    }

    fn posted(port: u16, body: &str, kind: &str) -> String {
        format!(
            "POST /answer HTTP/1.1\r\nHost: localhost:{port}\r\nX-Atelier-Key: {KEY}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn takes_the_shortcuts_address() {
        let address =
            Address::parse("http://127.0.0.1:24680/#123456789987654321111111111").unwrap();
        assert_eq!(address.port, 24680);
        assert_eq!(address.key, "123456789987654321111111111");
        assert!(!format!("{address:?}").contains("1234"));
        for wrong in [
            "http://127.0.0.1:24680/",
            "http://127.0.0.1:24680/#short",
            "http://localhost:24680/#123456789987654321111111111",
            "https://127.0.0.1:24680/#123456789987654321111111111",
            "http://127.0.0.1:80/#123456789987654321111111111",
            "http://127.0.0.1:99999/#123456789987654321111111111",
            "http://127.0.0.1:24680/#12345678998765432111111111 1",
            "http://127.0.0.1:24680/#1234567899876543211111111%2F",
        ] {
            assert!(Address::parse(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn serves_its_files_to_anyone_and_the_state_only_with_the_key() {
        let mut page = page();
        let (status, body) = get(&mut page, "/", None);
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(body.contains("Install Atelier"));
        assert_eq!(get(&mut page, "/page.js", None).0, "HTTP/1.1 200 OK");
        assert_eq!(get(&mut page, "/state", None).0, "HTTP/1.1 403 Forbidden");
        assert_eq!(
            get(&mut page, "/state", Some("123456789123456789123456780")).0,
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(page.fetched, 0);
        assert!(!page.visited);
        let (status, body) = get(&mut page, "/state", Some(KEY));
        assert!(page.visited);
        assert_eq!(status, "HTTP/1.1 200 OK");
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["seq"], 1);
        assert_eq!(state["view"]["screen"], "busy");
        assert_eq!(page.fetched, 1);
        assert_eq!(
            get(&mut page, "/elsewhere", Some(KEY)).0,
            "HTTP/1.1 404 Not Found"
        );
    }

    /// Sends the Shortcut's question about pairing, and has the page see it.
    fn ask_pairing(page: &mut Page, known: &str, key: &str) -> crate::net::Stream {
        let port = page.port;
        let sent = browser(
            port,
            format!(
                "GET /pairing/{known} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Atelier-Key: {key}\r\n\r\n"
            )
            .as_bytes(),
        );
        page.serve_waiting();
        sent
    }

    fn news(sent: crate::net::Stream) -> Value {
        let (status, body) = read_answer(sent);
        assert_eq!(status, "HTTP/1.1 200 OK");
        serde_json::from_str(&body).unwrap()
    }

    fn on_pair(page: &mut Page) {
        page.set(
            View::Pair {
                code: "482913".into(),
                problem: None,
            },
            false,
        );
    }

    #[test]
    fn the_shortcut_hears_when_settings_asks_and_once_paired() {
        let mut page = page();
        on_pair(&mut page);
        assert_eq!(page.pairing, Pairing::Waiting);
        let sent = ask_pairing(&mut page, "waiting", KEY);
        assert_eq!(page.held.len(), 1);
        page.set_pairing(Pairing::Asked);
        assert!(page.held.is_empty());
        let heard = news(sent);
        assert_eq!(heard["pairing"], "asked");
        assert_eq!(heard["changed"], true);
        assert_eq!(heard["next"], "notify");
        assert_eq!(heard["code"], "482913");
        assert_eq!(heard["notice"], "Code for Settings: 482 913");

        let sent = ask_pairing(&mut page, "asked", KEY);
        assert_eq!(page.held.len(), 1);
        page.set_pairing(Pairing::Paired);
        let heard = news(sent);
        assert_eq!(heard["pairing"], "paired");
        assert_eq!(heard["changed"], true);
        assert_eq!(heard["next"], "return");
        assert!(heard.get("code").is_none());
        // Nothing left to wait for.
        let heard = news(ask_pairing(&mut page, "paired", KEY));
        assert_eq!(heard["pairing"], "paired");
        assert_eq!(heard["changed"], false);
    }

    #[test]
    fn the_shortcut_hears_of_no_news_in_time() {
        let mut page = page();
        on_pair(&mut page);
        let sent = ask_pairing(&mut page, "waiting", KEY);
        assert_eq!(page.held.len(), 1);
        page.serve_waiting();
        assert_eq!(page.held.len(), 1);
        page.held[0].until = Instant::now();
        page.serve_waiting();
        let heard = news(sent);
        assert_eq!(heard["pairing"], "waiting");
        assert_eq!(heard["changed"], false);
        assert_eq!(heard["next"], "refresh");
        assert!(heard.get("notice").is_none());
    }

    #[test]
    fn the_shortcut_hears_at_once_what_it_doesnt_know() {
        let mut page = page();
        // Before Pair.
        let heard = news(ask_pairing(&mut page, "waiting", KEY));
        assert_eq!(heard["pairing"], "none");
        assert_eq!(heard["changed"], true);
        assert_eq!(heard["next"], "return");
        on_pair(&mut page);
        let heard = news(ask_pairing(&mut page, "none", KEY));
        assert_eq!(heard["pairing"], "waiting");
        assert_eq!(heard["changed"], true);
        assert_eq!(heard["next"], "ask");
        let heard = get(&mut page, "/pairing", Some(KEY)).1;
        let heard: Value = serde_json::from_str(&heard).unwrap();
        assert_eq!(heard["pairing"], "waiting");
        assert!(page.held.is_empty());
    }

    #[test]
    fn only_the_shortcut_asks_about_pairing() {
        let mut page = page();
        on_pair(&mut page);
        let (status, _) = read_answer(ask_pairing(
            &mut page,
            "waiting",
            "123456789123456789123456780",
        ));
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert_eq!(get(&mut page, "/pairing", None).0, "HTTP/1.1 403 Forbidden");
        assert!(page.held.is_empty());
        assert!(!page.in_settings);
    }

    #[test]
    fn a_stop_ends_the_shortcuts_wait() {
        let mut page = page();
        on_pair(&mut page);
        let sent = ask_pairing(&mut page, "waiting", KEY);
        page.set(
            View::Stopped {
                what: "No.".into(),
                next: Vec::new(),
            },
            false,
        );
        let heard = news(sent);
        assert_eq!(heard["pairing"], "none");
        assert_eq!(heard["changed"], true);
    }

    #[test]
    fn pair_says_to_go_back_once_youve_been_to_settings() {
        let mut page = page();
        on_pair(&mut page);
        let (_, body) = get(&mut page, "/state", Some(KEY));
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["view"]["problem"], Value::Null);
        let sent = ask_pairing(&mut page, "waiting", KEY);
        page.held[0].until = Instant::now();
        page.serve_waiting();
        news(sent);
        let seq = page.seq;
        let (_, body) = get(&mut page, "/state", Some(KEY));
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["view"]["problem"], NOT_YET);
        assert_eq!(state["seq"], seq + 1);
        // A try that didn't pair says so in its own words.
        page.set(
            View::Pair {
                code: "482913".into(),
                problem: Some("That didn't pair.".into()),
            },
            false,
        );
        let (_, body) = get(&mut page, "/state", Some(KEY));
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["view"]["problem"], "That didn't pair.");
    }

    #[test]
    fn answers_only_under_its_own_name() {
        let mut page = page();
        let request = format!(
            "GET / HTTP/1.1\r\nHost: atelier.example:{}\r\n\r\n",
            page.port
        );
        assert_eq!(exchange(&mut page, &request).0, "HTTP/1.1 403 Forbidden");
        assert_eq!(
            exchange(&mut page, "GET / HTTP/1.1\r\n\r\n").0,
            "HTTP/1.1 403 Forbidden"
        );
        assert_eq!(
            exchange(&mut page, "\x16\x03\x01\x02\x00\x01\x00\x01\r\n\r\n").0,
            "HTTP/1.1 400 Bad Request"
        );
    }

    #[test]
    fn takes_one_answer_to_the_view_it_asked() {
        let mut page = page();
        page.set(
            View::SignIn {
                password: true,
                problem: None,
            },
            true,
        );
        let (seq, port) = (page.seq, page.port);
        let answer =
            format!(r#"{{"seq":{seq},"answer":{{"apple_id":"a@b.c","password":"secret"}}}}"#);
        // Not as a form, which any page can send.
        assert_eq!(
            exchange(&mut page, &posted(port, &answer, "text/plain")).0,
            "HTTP/1.1 415 Unsupported Media Type"
        );
        let stale = format!(r#"{{"seq":{},"answer":{{}}}}"#, seq - 1);
        assert_eq!(
            exchange(&mut page, &posted(port, &stale, JSON)).1,
            r#"{"taken":false}"#
        );
        assert_eq!(
            exchange(&mut page, &posted(port, &answer, JSON)).1,
            r#"{"taken":true}"#
        );
        assert_eq!(
            exchange(&mut page, &posted(port, &answer, JSON)).1,
            r#"{"taken":false}"#
        );
        let taken = page.answer.take().unwrap();
        assert_eq!(taken.text("apple_id"), "a@b.c");
        assert_eq!(taken.text("password"), "secret");
        assert_eq!(taken.text("code"), "");
        assert!(!taken.yes());
    }

    #[test]
    fn stops_when_the_page_asks_with_the_key() {
        let mut page = page();
        page.set(
            View::Pair {
                code: "123456".into(),
                problem: None,
            },
            false,
        );
        let stop = |port: u16, key: &str| {
            format!(
                "POST /stop HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Atelier-Key: {key}\r\nContent-Length: 0\r\n\r\n"
            )
        };
        let port = page.port;
        assert_eq!(
            exchange(&mut page, &stop(port, "123456789123456789123456780")).0,
            "HTTP/1.1 403 Forbidden"
        );
        assert!(!page.ending);
        let (status, body) = exchange(&mut page, &stop(port, KEY));
        assert_eq!(status, "HTTP/1.1 200 OK");
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["view"]["screen"], "ended");
        assert_eq!(state["asking"], false);
        assert!(page.ending);

        // Done is done: nothing to stop.
        let mut page = super::tests::page();
        page.set(
            View::Done {
                release: "Atelier 1.0".into(),
                handed: true,
            },
            false,
        );
        let port = page.port;
        let (_, body) = exchange(&mut page, &stop(port, KEY));
        let state: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(state["view"]["screen"], "done");
        assert!(!page.ending);
    }

    #[test]
    fn asks_and_waits_for_the_answer() {
        let mut page = page();
        let (port, asked) = (page.port, page.seq + 1);
        // The page reads the question, then answers it.
        let reads = browser(
            port,
            format!(
                "GET /state HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Atelier-Key: {KEY}\r\n\r\n"
            )
            .as_bytes(),
        );
        let body = format!(r#"{{"seq":{asked},"answer":{{"yes":true,"pick":1}}}}"#);
        let answers = browser(port, posted(port, &body, JSON).as_bytes());
        let answer = page
            .ask(View::Question {
                title: "Which team signs Atelier?".into(),
                text: vec![],
                options: vec!["One".into(), "Two".into()],
                yes: "Continue".into(),
                no: Some("Stop".into()),
            })
            .unwrap();
        assert!(answer.yes());
        assert_eq!(answer.pick(), Some(1));
        assert!(!page.asking);
        let state: Value = serde_json::from_str(&read_answer(reads).1).unwrap();
        assert_eq!(state["view"]["screen"], "question");
        assert_eq!(state["asking"], true);
        assert_eq!(read_answer(answers).1, r#"{"taken":true}"#);
    }

    #[test]
    fn a_bar_moves_the_views_number_only_when_it_changes() {
        let mut page = page();
        page.set(
            View::Installing {
                signed: false,
                bar: Bar::new(0.0, "Signing Atelier"),
            },
            false,
        );
        let seq = page.seq;
        PAGE.set(Some(page));
        bar(0.004, "Signing Atelier");
        assert_eq!(with(|page| page.seq), Some(seq));
        bar(0.5, "Signing Atelier");
        assert_eq!(with(|page| page.seq), Some(seq + 1));
        let state: Value = serde_json::from_slice(&with(|page| page.state()).unwrap()).unwrap();
        assert_eq!(state["stage"], 2);
        assert_eq!(state["view"]["bar"]["percent"], 50);
        PAGE.set(None);
    }

    #[test]
    fn says_a_stop_as_the_page_does() {
        let said = |stop: Stop| match stopped_view(&stop) {
            View::Stopped { what, next } => (what, next),
            _ => unreachable!(),
        };
        assert_eq!(
            said(Stop::new("Stopped: nothing was signed or installed")),
            ("Nothing was signed or installed.".into(), vec![])
        );
        assert_eq!(
            said(
                Stop::new("The iPhone didn't install it: no reason given.").next(
                    "Run this again to try the install again, or with --redo to sign it anew first."
                )
            ),
            (
                "The iPhone didn't install it: no reason given.".into(),
                vec!["Run the Shortcut again to try the install again.".into()]
            )
        );
        assert_eq!(
            said(
                Stop::new("LocalDevVPN isn't connected")
                    .next("Open LocalDevVPN and tap Connect, on Wi-Fi, then run this again.")
            )
            .1,
            vec!["Open LocalDevVPN and tap Connect, on Wi-Fi, then run the Shortcut again."]
        );
    }

    #[test]
    fn keys_are_compared_whole() {
        assert!(same(KEY, KEY));
        assert!(!same("123", KEY));
        assert!(!same(&KEY.replace('9', "8"), KEY));
    }
}
