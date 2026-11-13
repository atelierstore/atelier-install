//! Page test: a test for Atelier's installer, to run in rootshell (App
//! Store) on an iPhone. It isn't part of the installer.
//!
//! The installer asks its questions in rootshell's terminal. This tries
//! another way: the program serves a page of its own, which a Shortcut's
//! "Show Web Page" or Safari shows, and the page asks and answers. Two
//! things have to hold for that, and the test measures both on the iPhone:
//!
//! - a program rootshell runs can serve a page and take what's typed on it,
//!   one request after the other, with rootshell's calls, which all wait
//!   and can't be given a time limit (src/host.rs);
//! - rootshell goes on working at speed while that page is in front of it.
//!
//! So it listens on one port, does work that only needs the processor, in
//! pieces, as the installer's signing does, counts how much each second
//! saw, and serves a page that shows those counts and sends back a form.
//! It ends by itself, closing its port. rootshell doesn't close a port for
//! a program it stops (Control-C), and keeps what that program was waiting
//! for, so the port stays taken until rootshell is quit, and the next
//! program in the same tab can lose its first connection (see `Door`).

mod host;
mod request;
mod speed;

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use host::{Connection, Listener, Refused};
use request::{Nothing, Request};
use speed::{History, Work};

/// Which test this is, for telling one build's results from another's.
const VERSION: u32 = 1;

const PAGE: &str = include_str!("page.html");

const DEFAULT_PORT: u16 = 8626;
const DEFAULT_MINUTES: u64 = 10;
const MOST_MINUTES: u64 = 60;

/// WASI's error number for a port that's in use.
const PORT_IN_USE: i32 = 1;

const PORT_TAKEN: &str = "\
The port is taken: by this test still
running in another tab, or by a program
that was stopped with Control-C, whose port
rootshell keeps until it's quit.

Close the other tab, or quit rootshell from
the app switcher and open it again. Or add
--port 8627 to the command, for another port.";

/// How long the program works between two looks at its port, and how often
/// it checks in with rootshell meanwhile, as the installer does.
const WORK_MS: u64 = 150;
const CHECK_IN_MS: u64 = 50;

/// How long after a page last asked the test takes the page for closed.
/// Only time the program ran counts: see `Test::beat`.
const PAGE_CLOSED_MS: u64 = 30_000;

/// How many connections the program answers before it works again.
const ANSWERED_IN_A_ROW: usize = 8;

/// How often the terminal says how it goes, in seconds.
const SAID_EVERY: u64 = 5;

const USAGE: &str = "\
Usage: ./atelier-page-test.wasm [--port NUMBER] [--minutes NUMBER]

Serves a page at http://127.0.0.1:8626/ (or at the port given) and says how
fast rootshell works behind it, for 10 minutes (or the minutes given, 60 at
most).";

struct Options {
    port: u16,
    minutes: u64,
}

impl Options {
    fn from(mut arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Options {
            port: DEFAULT_PORT,
            minutes: DEFAULT_MINUTES,
        };
        while let Some(argument) = arguments.next() {
            let mut number = |least: u64, most: u64| {
                arguments
                    .next()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| (least..=most).contains(value))
                    .ok_or(format!("{argument} takes a number from {least} to {most}."))
            };
            match argument.as_str() {
                "--port" => options.port = number(1024, 65535)? as u16,
                "--minutes" => options.minutes = number(1, MOST_MINUTES)?,
                _ => return Err(format!("{argument} isn't something this test takes.")),
            }
        }
        Ok(options)
    }
}

fn main() -> ExitCode {
    let options = match Options::from(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(problem) => {
            println!("{problem}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    println!("Page test {VERSION}, for Atelier's installer.");
    println!("Starting on port {}, in a second or two…", options.port);
    match Test::start(options).and_then(Test::run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refused) => {
            println!();
            if refused.errno == PORT_IN_USE {
                println!("{PORT_TAKEN}");
            } else {
                println!("The test stopped: rootshell's {refused}.");
            }
            ExitCode::FAILURE
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// 16 bytes nobody else knows, from the system's randomness, which seeds
/// each `RandomState`.
fn random_bytes() -> [u8; WORD] {
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; WORD];
    for half in out.chunks_mut(8) {
        let word = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        half.copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// How long the word the program knocks with is, and a knock: the word,
/// then the knock's number.
const WORD: usize = 16;
const KNOCK: usize = WORD + 4;

/// How many times the program knocks before its first accept.
const FIRST_KNOCKS: u32 = 4;

/// The program's port, asked for a connection without waiting for one.
///
/// rootshell's accept waits until a connection comes, and nothing looks
/// first or wakes it. So before it accepts, the program connects to its own
/// port and sends a word nobody else knows, a knock: accept then returns at
/// once, with a browser's connection if one came first, since rootshell
/// hands them over in the order they came, or with the program's own, which
/// says nobody was waiting.
///
/// It knocks `FIRST_KNOCKS` times before its first accept, and numbers its
/// knocks. rootshell keeps an accept that a program was stopped in
/// (Control-C), under the socket's number, which the next program's
/// listener gets too, and gives that accept the listener's first
/// connection, which is lost: a knock that comes back says the ones before
/// it won't.
struct Door {
    listener: Listener,
    port: u16,
    word: [u8; WORD],
    /// How many times the program has knocked.
    knocked: u32,
    /// Its own connections that accept hasn't handed back, oldest first,
    /// each with its knock's number.
    ours: VecDeque<(u32, Connection)>,
    /// A browser's connection that came before the first knock came back.
    early: Option<Next>,
}

enum Next {
    Nobody,
    /// A connection, what it sent first (nothing if it closed without a
    /// word), and how long that took to come.
    Somebody {
        connection: Connection,
        first: Vec<u8>,
        waited_ms: u64,
    },
}

impl Door {
    /// Listens on `port`, and returns once a connection to it has come
    /// back: the port is this program's. It's another listener's when a
    /// knock can't be sent, since that one drops what connects: one that
    /// rootshell never closed, or another tab's.
    fn open(port: u16) -> Result<Self, Refused> {
        let taken = Refused {
            call: "listen",
            errno: PORT_IN_USE,
        };
        let listener = Listener::on(port)?;
        // rootshell answers `listen` before its listener is ready.
        pause(300);
        let mut door = Door {
            listener,
            port,
            word: random_bytes(),
            knocked: 0,
            ours: VecDeque::new(),
            early: None,
        };
        let mut connected = door.connect();
        for _ in 0..10 {
            if connected.is_ok() {
                break;
            }
            pause(100);
            connected = door.connect();
        }
        connected?;
        for _ in 1..FIRST_KNOCKS {
            door.connect()?;
        }
        for (number, ours) in &mut door.ours {
            let mut knock = [0u8; KNOCK];
            knock[..WORD].copy_from_slice(&door.word);
            knock[WORD..].copy_from_slice(&number.to_be_bytes());
            ours.send(&knock).map_err(|_| taken)?;
        }
        // A connection that was dropped takes what's sent once, then not.
        for _ in 0..4 {
            pause(80);
            for (_, ours) in &mut door.ours {
                ours.send(b"\n").map_err(|_| taken)?;
            }
        }
        if let somebody @ Next::Somebody { .. } = door.accept()? {
            door.early = Some(somebody);
        }
        Ok(door)
    }

    /// Connects to the program's own port, for a knock.
    fn connect(&mut self) -> Result<(), Refused> {
        let ours = Connection::to_this_iphone(self.port)?;
        self.knocked += 1;
        self.ours.push_back((self.knocked, ours));
        Ok(())
    }

    /// The next connection, if one is waiting.
    fn next(&mut self) -> Result<Next, Refused> {
        if let Some(early) = self.early.take() {
            return Ok(early);
        }
        if self.ours.is_empty() {
            self.connect()?;
            let mut knock = [0u8; KNOCK];
            knock[..WORD].copy_from_slice(&self.word);
            knock[WORD..].copy_from_slice(&self.knocked.to_be_bytes());
            if let Some((_, ours)) = self.ours.back_mut() {
                ours.send(&knock)?;
            }
        }
        self.accept()
    }

    /// Accepts until a browser's connection comes, or the last of the
    /// program's own does.
    fn accept(&mut self) -> Result<Next, Refused> {
        loop {
            let mut connection = self.listener.accept()?;
            let accepted_ms = now_ms();
            let mut first = vec![0u8; 4096];
            // A connection that breaks is one that closed.
            let mut got = connection.receive(&mut first).unwrap_or(0);
            // A knock, if it's that, may come in pieces.
            while got > 0 && got < KNOCK && first[..got.min(WORD)] == self.word[..got.min(WORD)] {
                match connection.receive(&mut first[got..]) {
                    Ok(more) if more > 0 => got += more,
                    _ => break,
                }
            }
            if got >= KNOCK && first[..WORD] == self.word {
                let mut number = [0u8; 4];
                number.copy_from_slice(&first[WORD..KNOCK]);
                let number = u32::from_be_bytes(number);
                // It's back, and the ones before it never will be.
                while self.ours.front().is_some_and(|(ours, _)| *ours <= number) {
                    self.ours.pop_front();
                }
                if self.ours.is_empty() {
                    return Ok(Next::Nobody);
                }
                continue;
            }
            first.truncate(got);
            return Ok(Next::Somebody {
                connection,
                first,
                waited_ms: now_ms().saturating_sub(accepted_ms),
            });
        }
    }
}

/// Why the test ended.
enum Over {
    Asked,
    PageClosed,
    TimeUp,
}

#[derive(Default)]
struct Counts {
    /// Times the page was opened, and times it asked how it goes.
    pages: u64,
    asked: u64,
    /// Forms the page sent.
    answers: u64,
    /// Connections that closed without a whole request, and the longest
    /// the program waited on one.
    silent: u64,
    silent_ms: u64,
    /// Connections that sent something else than HTTP.
    strange: u64,
    /// Requests that weren't the page's own.
    turned_away: u64,
}

struct Test {
    options: Options,
    door: Door,
    work: Work,
    history: History,
    started_ms: u64,
    /// When the program last got somewhere, and the longest it went without:
    /// since the start, and since the terminal's last line.
    beat_ms: u64,
    longest_pause_ms: u64,
    pause_lately_ms: u64,
    /// How long looking at the port took, the last times nobody was there.
    knocks_ms: VecDeque<u64>,
    /// How long the program has waited for rootshell to answer its calls
    /// (looking at the port, checking in) since the terminal's last line,
    /// when that line was, and the share of the time those calls took over
    /// the line before.
    calls_ms: u64,
    said_ms: u64,
    waiting: Option<u8>,
    counts: Counts,
    /// How long the program has run since a page last asked.
    unasked_ms: u64,
    /// The seconds the terminal has said up to, and the lowest share of the
    /// best second it said.
    said_seconds: u64,
    slowest: Option<u8>,
}

impl Test {
    fn start(options: Options) -> Result<Self, Refused> {
        let door = Door::open(options.port)?;
        println!();
        println!("The page: http://127.0.0.1:{}/", options.port);
        println!();
        println!("It ends with the page's Stop button,");
        println!("30 seconds after the page closes,");
        println!("or after {}.", minutes(options.minutes));
        println!();
        println!("Each line: the last {SAID_EVERY} seconds' speed,");
        println!("as a share of the best second so far.");
        println!();
        let now = now_ms();
        Ok(Test {
            options,
            door,
            work: Work::new(now),
            history: History::starting_at(now),
            started_ms: now,
            beat_ms: now,
            longest_pause_ms: 0,
            pause_lately_ms: 0,
            knocks_ms: VecDeque::with_capacity(KNOCKS_KEPT),
            calls_ms: 0,
            said_ms: now,
            waiting: None,
            counts: Counts::default(),
            unasked_ms: 0,
            said_seconds: 0,
            slowest: None,
        })
    }

    fn run(mut self) -> Result<(), Refused> {
        let over = 'test: loop {
            // Everyone who is waiting, then work.
            for _ in 0..ANSWERED_IN_A_ROW {
                let before = now_ms();
                match self.door.next()? {
                    Next::Nobody => {
                        let now = now_ms();
                        if self.knocks_ms.len() == KNOCKS_KEPT {
                            self.knocks_ms.pop_front();
                        }
                        self.knocks_ms.push_back(now.saturating_sub(before));
                        self.calls_ms += now.saturating_sub(before);
                        self.beat(now);
                        break;
                    }
                    Next::Somebody {
                        connection,
                        first,
                        waited_ms,
                    } => {
                        let over = self.serve(connection, first, waited_ms);
                        // Waiting on a browser isn't a pause of rootshell's.
                        self.beat_ms = now_ms();
                        if let Some(over) = over {
                            break 'test over;
                        }
                    }
                }
            }
            self.work();
            self.say_how_it_goes();
            if let Some(over) = self.over() {
                break over;
            }
        };
        self.say_the_end(over);
        Ok(())
    }

    /// Notes that the program got somewhere, at `now`. The time since it
    /// last did is a pause when it's long, and counts toward the page
    /// being closed for half a second at most: what's longer, the program
    /// wasn't running, and no page could have asked.
    fn beat(&mut self, now: u64) {
        let gap = now.saturating_sub(self.beat_ms);
        self.longest_pause_ms = self.longest_pause_ms.max(gap);
        self.pause_lately_ms = self.pause_lately_ms.max(gap);
        self.unasked_ms += gap.min(500);
        self.beat_ms = now;
    }

    /// Works for `WORK_MS`, checking in with rootshell as it goes.
    fn work(&mut self) {
        let began = now_ms();
        let mut checked_in = began;
        loop {
            let steps = self.work.piece();
            let now = now_ms();
            self.history.add(now, steps);
            self.beat(now);
            if now.abs_diff(checked_in) >= CHECK_IN_MS {
                host::check_in();
                checked_in = now_ms();
                self.calls_ms += checked_in.saturating_sub(now);
            }
            if now.abs_diff(began) >= WORK_MS {
                break;
            }
        }
    }

    fn over(&self) -> Option<Over> {
        if now_ms().saturating_sub(self.started_ms) >= self.options.minutes * 60_000 {
            Some(Over::TimeUp)
        } else if self.counts.pages + self.counts.asked > 0 && self.unasked_ms >= PAGE_CLOSED_MS {
            Some(Over::PageClosed)
        } else {
            None
        }
    }

    /// Answers one connection. Says the test is over when the page asked
    /// for that.
    fn serve(
        &mut self,
        mut connection: Connection,
        first: Vec<u8>,
        waited_ms: u64,
    ) -> Option<Over> {
        let began = now_ms();
        let read = request::read(first, &mut |buf| connection.receive(buf).unwrap_or(0));
        let request = match read {
            Ok(request) => request,
            Err(Nothing::Silent | Nothing::CutShort) => {
                // As long as it took to close: before its first bytes, if
                // it sent any, and after them.
                let waited_ms = waited_ms + now_ms().saturating_sub(began);
                self.counts.silent += 1;
                self.counts.silent_ms = self.counts.silent_ms.max(waited_ms);
                if waited_ms >= 1000 {
                    self.say(&format!(
                        "a connection said nothing for {}",
                        seconds(waited_ms)
                    ));
                }
                return None;
            }
            Err(Nothing::NotHttp | Nothing::TooLong) => {
                self.counts.strange += 1;
                if self.counts.strange == 1 {
                    self.say("a connection sent something that isn't a page's request");
                }
                let _ = connection.send(&request::answer(
                    "400 Bad Request",
                    TEXT,
                    b"This is plain HTTP, for the test's own page.\n",
                ));
                return None;
            }
        };

        let mut over = None;
        let answer = match self.route(&request) {
            Route::Page => {
                self.counts.pages += 1;
                self.unasked_ms = 0;
                self.say("the page opened");
                request::answer("200 OK", "text/html; charset=utf-8", PAGE.as_bytes())
            }
            Route::Status => {
                self.counts.asked += 1;
                self.unasked_ms = 0;
                request::answer("200 OK", JSON, self.status().as_bytes())
            }
            Route::Answer => {
                let fields = request::form(&request.body);
                let length = |name: &str| {
                    fields
                        .iter()
                        .find(|(field, _)| field == name)
                        .map_or(0, |(_, value)| value.chars().count())
                };
                // Only how long each is: what was typed is said nowhere.
                let (first, second) = (length("first"), length("second"));
                self.counts.answers += 1;
                self.say(&format!(
                    "the page sent an answer: {first} and {second} characters"
                ));
                let body = format!("{{\"first\":{first},\"second\":{second}}}");
                request::answer("200 OK", JSON, body.as_bytes())
            }
            Route::Stop => {
                over = Some(Over::Asked);
                request::answer("200 OK", JSON, b"{\"over\":true}")
            }
            Route::NotThere => request::answer("404 Not Found", TEXT, b"Nothing here.\n"),
            Route::TurnedAway(why) => {
                self.counts.turned_away += 1;
                request::answer("403 Forbidden", TEXT, why.as_bytes())
            }
        };
        let _ = connection.send(&answer);
        over
    }

    fn route(&self, request: &Request) -> Route {
        // A page elsewhere can have a browser ask this port under another
        // name, or post to it: neither is the test's own page.
        let port = self.options.port;
        let here = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        if !request
            .header("host")
            .is_some_and(|host| here.iter().any(|name| name == host))
        {
            return Route::TurnedAway(format!(
                "This page only opens on the iPhone itself, at http://127.0.0.1:{port}/\n"
            ));
        }
        let from_the_page = request.header("x-page-test") == Some("1");
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => Route::Page,
            ("GET", "/status") => Route::Status,
            ("POST", "/answer" | "/stop") if !from_the_page => {
                Route::TurnedAway("Only the test's own page sends this.\n".to_string())
            }
            ("POST", "/answer") => Route::Answer,
            ("POST", "/stop") => Route::Stop,
            _ => Route::NotThere,
        }
    }

    /// How it goes, as the page reads it.
    fn status(&self) -> String {
        let now = self.history.lately(3).map_or("null".to_string(), |steps| {
            self.history.percent(steps).to_string()
        });
        let waiting = self
            .waiting
            .map_or("null".to_string(), |share| share.to_string());
        let knock = if self.knocks_ms.is_empty() {
            "null".to_string()
        } else {
            let sum: u64 = self.knocks_ms.iter().sum();
            format!("{:.1}", sum as f64 / self.knocks_ms.len() as f64)
        };
        let mut chart = String::new();
        for (index, share) in self.history.chart().iter().enumerate() {
            if index > 0 {
                chart.push(',');
            }
            let _ = write!(chart, "{share}");
        }
        format!(
            "{{\"test\":{VERSION},\"port\":{},\"minutes\":{},\"running_s\":{},\"now\":{now},\
             \"best\":{},\"pause_ms\":{},\"knock_ms\":{knock},\"waiting\":{waiting},\"pages\":{},\"asked\":{},\
             \"answers\":{},\"silent\":{},\"silent_ms\":{},\"strange\":{},\"turned_away\":{},\
             \"chart\":[{chart}]}}",
            self.options.port,
            self.options.minutes,
            now_ms().saturating_sub(self.started_ms) / 1000,
            self.history.best(),
            self.longest_pause_ms,
            self.counts.pages,
            self.counts.asked,
            self.counts.answers,
            self.counts.silent,
            self.counts.silent_ms,
            self.counts.strange,
            self.counts.turned_away,
        )
    }

    /// A line in the terminal, with how long the test has run.
    fn say(&self, what: &str) {
        let running = now_ms().saturating_sub(self.started_ms) / 1000;
        println!("{:>6}  {what}", clock(running));
    }

    /// A line every `SAID_EVERY` seconds: the speed over those, and the
    /// longest pause in them when there was one to speak of.
    fn say_how_it_goes(&mut self) {
        let seconds_over = self.history.seconds();
        if seconds_over < self.said_seconds + SAID_EVERY {
            return;
        }
        self.said_seconds = seconds_over;
        let lately = self.history.lately(SAID_EVERY as usize).unwrap_or(0);
        let share = self.history.percent(lately);
        self.slowest = Some(self.slowest.map_or(share, |slowest| slowest.min(share)));
        let now = now_ms();
        let waiting = share_of(self.calls_ms, now.saturating_sub(self.said_ms));
        self.waiting = Some(waiting);
        let mut line = format!("speed {share}%");
        if self.pause_lately_ms >= 1000 {
            let _ = write!(line, ", paused {}", seconds(self.pause_lately_ms));
        }
        if waiting >= 20 {
            let _ = write!(line, ", waiting on rootshell {waiting}%");
        }
        self.pause_lately_ms = 0;
        self.calls_ms = 0;
        self.said_ms = now;
        println!("{:>6}  {line}", clock(seconds_over));
    }

    fn say_the_end(&self, over: Over) {
        println!();
        println!(
            "The test is over: {}.",
            match over {
                Over::Asked => "the page asked to stop".to_string(),
                Over::PageClosed => "the page has been closed for 30 seconds".to_string(),
                Over::TimeUp => format!("it has run for {}", minutes(self.options.minutes)),
            }
        );
        println!("Best second: {}.", millions(self.history.best()));
        if let Some(slowest) = self.slowest {
            println!("Slowest {SAID_EVERY} seconds: {slowest}% of that.");
        }
        println!("Longest pause: {}.", seconds(self.longest_pause_ms));
        println!(
            "The page opened {} and asked {}.",
            times(self.counts.pages),
            times(self.counts.asked)
        );
        if self.counts.silent > 0 {
            println!(
                "Connections that said nothing: {}, the longest for {}.",
                self.counts.silent,
                seconds(self.counts.silent_ms)
            );
        }
    }
}

/// How many of its last looks at the port the test averages.
const KNOCKS_KEPT: usize = 16;

const JSON: &str = "application/json";
const TEXT: &str = "text/plain; charset=utf-8";

enum Route {
    Page,
    Status,
    Answer,
    Stop,
    NotThere,
    TurnedAway(String),
}

/// `part` of `whole` as a share of 100, which it stays within.
fn share_of(part: u64, whole: u64) -> u8 {
    if whole == 0 {
        return 0;
    }
    (part.saturating_mul(100) / whole).min(100) as u8
}

/// 75 seconds as `1:15`.
fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// 4,200 ms as `4.2 s`.
fn seconds(ms: u64) -> String {
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

/// 41,200,000 as `41.2 million steps`.
fn millions(steps: u64) -> String {
    format!(
        "{}.{} million steps",
        steps / 1_000_000,
        steps % 1_000_000 / 100_000
    )
}

fn minutes(count: u64) -> String {
    match count {
        1 => "1 minute".to_string(),
        count => format!("{count} minutes"),
    }
}

fn times(count: u64) -> String {
    match count {
        1 => "1 time".to_string(),
        count => format!("{count} times"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(arguments: &[&str]) -> Result<Options, String> {
        Options::from(arguments.iter().map(|argument| argument.to_string()))
    }

    #[test]
    fn takes_a_port_and_minutes() {
        let defaults = options(&[]).unwrap();
        assert_eq!((defaults.port, defaults.minutes), (8626, 10));
        let given = options(&["--minutes", "2", "--port", "50123"]).unwrap();
        assert_eq!((given.port, given.minutes), (50123, 2));
    }

    #[test]
    fn says_what_it_doesnt_take() {
        assert_eq!(
            options(&["--port", "80"]).err().unwrap(),
            "--port takes a number from 1024 to 65535."
        );
        assert_eq!(
            options(&["--minutes"]).err().unwrap(),
            "--minutes takes a number from 1 to 60."
        );
        assert_eq!(
            options(&["--fast"]).err().unwrap(),
            "--fast isn't something this test takes."
        );
    }

    #[test]
    fn writes_times_and_counts() {
        assert_eq!(clock(5), "0:05");
        assert_eq!(clock(75), "1:15");
        assert_eq!(seconds(4_260), "4.2 s");
        assert_eq!(seconds(90), "0.0 s");
        assert_eq!(millions(41_250_000), "41.2 million steps");
        assert_eq!(times(1), "1 time");
        assert_eq!(times(0), "0 times");
        assert_eq!(share_of(250, 5_000), 5);
        assert_eq!(share_of(9_000, 5_000), 100);
        assert_eq!(share_of(1, 0), 0);
        assert_eq!(minutes(1), "1 minute");
        assert_eq!(minutes(10), "10 minutes");
    }

    #[test]
    fn the_word_it_knocks_with_differs_each_time() {
        assert_ne!(random_bytes(), random_bytes());
        let word = random_bytes();
        assert_ne!(word[..8], word[8..]);
    }
}
