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
    /// Installed; `handed` when Atelier has the pairing and the sign-in.
    Done { handed: bool },
    /// Why the program stopped, and what to do.
    Stopped { what: String, next: Vec<String> },
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
            View::Busy { .. } | View::Question { .. } | View::Stopped { .. } => None,
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
    terminal::log(format!("the page: port {}", address.port));
    PAGE.set(Some(page));
    Ok(())
}

/// Whether the program has a page to show.
pub fn on() -> bool {
    PAGE.with_borrow(Option::is_some)
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

/// Shows why the program stopped.
pub fn stopped(stop: &Stop) {
    show(View::Stopped {
        what: format!("{}.", stop.what.trim_end_matches('.')),
        next: stop.next.clone(),
    });
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
        })
    }

    fn set(&mut self, view: View, asking: bool) {
        if let Some(stage) = view.stage() {
            self.stage = stage;
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

    fn serve_waiting(&mut self) {
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
        let reply = match http::read(&mut visitor) {
            Ok(request) => self.reply(&request),
            Err(Nothing::Silent | Nothing::CutShort) => return,
            Err(Nothing::NotHttp | Nothing::TooLong) => http::answer(
                "400 Bad Request",
                TEXT,
                b"This is the installer's page, over plain HTTP.\n",
            ),
        };
        let _ = visitor.write_all(&reply);
    }

    fn reply(&mut self, request: &Request) -> Vec<u8> {
        let port = self.port;
        let here = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        if !request
            .header("host")
            .is_some_and(|host| here.iter().any(|name| name == host))
        {
            let only =
                format!("This page only opens at http://127.0.0.1:{port}/ on this iPhone.\n");
            return http::answer("403 Forbidden", TEXT, only.as_bytes());
        }
        let keyed = request
            .header("x-atelier-key")
            .is_some_and(|key| same(key, &self.key));
        let file = |kind: &str, body: &[u8]| http::answer("200 OK", kind, body);
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => file("text/html; charset=utf-8", INDEX),
            ("GET", "/page.css") => file("text/css; charset=utf-8", STYLE),
            ("GET", "/page.js") => file("text/javascript; charset=utf-8", SCRIPT),
            ("GET", "/icon.png") => file("image/png", ICON),
            ("GET", "/display.woff2") => file("font/woff2", FONT),
            ("GET", "/state") | ("POST", "/answer") if !keyed => {
                http::answer("403 Forbidden", TEXT, b"Only the page asks this.\n")
            }
            ("GET", "/state") => {
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
        let (status, body) = get(&mut page, "/state", Some(KEY));
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
    fn keys_are_compared_whole() {
        assert!(same(KEY, KEY));
        assert!(!same("123", KEY));
        assert!(!same(&KEY.replace('9', "8"), KEY));
    }
}
