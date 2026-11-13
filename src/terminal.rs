//! What the program says and asks, in rootshell's terminal, and the log it
//! keeps for a run that fails on the iPhone. rootshell's terminal has no
//! termios, so the password is typed in its raw mode, which hides it.

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Why the program stops, and what to do about it.
#[derive(Debug)]
pub struct Stop {
    pub what: String,
    pub next: Vec<String>,
}

impl Stop {
    pub fn new(what: impl Into<String>) -> Self {
        Stop {
            what: what.into(),
            next: Vec::new(),
        }
    }

    /// Adds a line on what to do next.
    pub fn next(mut self, line: impl Into<String>) -> Self {
        self.next.push(line.into());
        self
    }
}

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Self {
        Stop::new(error.to_string())
    }
}

static LOG: Mutex<Option<File>> = Mutex::new(None);
/// When the log began, for how long each step took.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Starts the log at `path`, replacing the last run's. It holds the steps
/// and the errors, never the Apple ID, its password, a code, the name of
/// the Apple ID's owner, the iPhone's name, the team ID or a phone number
/// (`say_personal`).
pub fn start_log(path: &Path) {
    STARTED.get_or_init(Instant::now);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path);
    if let Ok(file) = file {
        *LOG.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(file);
    }
}

/// A line for the log only.
pub fn log(line: impl Display) {
    if let Some(file) = LOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_mut()
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Posts `text` as a notification: the terminal takes OSC 777 for one and
/// prints nothing. rootshell (92ded20) doesn't count a running .wasm program
/// as a session, so behind another app it keeps the notification until it's
/// in front again, where it skips it for the focused terminal: for now it
/// only reaches a tab that isn't on screen. Not in the log, as it can hold
/// the pairing's code.
pub fn notify(text: &str) {
    print!("\x1b]777;notify;Atelier;{text}\x07");
    let _ = io::stdout().flush();
}

/// A numbered step's heading; in the log, with the seconds since the log
/// began, which tell how long each step took.
pub fn step(number: u32, of: u32, title: &str) {
    println!();
    println!("[{number}/{of}] {title}");
    let seconds = STARTED
        .get()
        .map_or(0, |started| started.elapsed().as_secs());
    log(format!("[{number}/{of}] {title} ({seconds} s in)"));
}

/// A line under a step.
pub fn say(line: impl Display) {
    println!("  {line}");
    log(format!("  {line}"));
}

/// A line under a step that holds something of the user's, such as the
/// iPhone's name, the team ID or a phone number: the log, which people
/// share, gets `logged` in its place.
pub fn say_personal(line: impl Display, logged: impl Display) {
    println!("  {line}");
    log(format!("  {logged}"));
}

/// Back to the start of the line, then the line, then nothing after it.
/// rootshell turns a carriage return in a program's output into a new line,
/// so the line goes back with the terminal's own code for it (CSI G).
const OVER: &str = "\x1b[G";
const CLEAR_REST: &str = "\x1b[K";

/// A line that the next one replaces, for progress. It ends with `done`.
pub fn progress(line: impl Display) {
    print!("{OVER}  {line}{CLEAR_REST}");
    let _ = io::stdout().flush();
}

/// Progress as a bar, `fraction` of the way along, with `label` after it,
/// as in `███████████░░░░░░░  46 of 67 MB`. It ends with `done`.
pub fn bar(fraction: f64, label: impl Display) {
    let columns = std::env::var("COLUMNS")
        .ok()
        .and_then(|columns| columns.parse().ok());
    progress(bar_line(fraction, &label.to_string(), columns));
}

/// The most cells the bar takes, and the fewest worth showing.
const BAR_CELLS: usize = 18;
const BAR_CELLS_MIN: usize = 6;

/// The bar and its label, in `columns` when rootshell says how many the
/// terminal has, as it does at the start: a line that wraps can't be gone
/// back over. Too narrow for a bar, it's the label alone.
fn bar_line(fraction: f64, label: &str, columns: Option<usize>) -> String {
    // The indent, the gap after the bar, and a column spare, since a line
    // that fills the last one wraps on some terminals.
    let room = columns.map_or(BAR_CELLS, |columns| {
        columns.saturating_sub(2 + 2 + 1 + label.chars().count())
    });
    let cells = room.min(BAR_CELLS);
    if cells < BAR_CELLS_MIN {
        return label.to_string();
    }
    let full = ((fraction.clamp(0.0, 1.0) * cells as f64) as usize).min(cells);
    format!("{}{}  {label}", "█".repeat(full), "░".repeat(cells - full))
}

/// Ends a line of progress.
pub fn done(line: impl Display) {
    println!("{OVER}  {line}{CLEAR_REST}");
    log(format!("  {line}"));
}

/// Asks for a line. What's typed isn't logged.
pub fn ask(prompt: &str) -> Result<String, Stop> {
    print!("  {prompt}");
    let _ = io::stdout().flush();
    read_line()
}

/// Asks for a line, with the prompt where a line starts, as --forget
/// does. What's typed isn't logged.
pub fn answer(prompt: &str) -> Result<String, Stop> {
    print!("{prompt}");
    let _ = io::stdout().flush();
    read_line()
}

/// Asks a yes or no question; anything but a yes is a no.
pub fn confirm(prompt: &str) -> Result<bool, Stop> {
    let answer = ask(&format!("{prompt} [y/N] "))?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}

/// Asks for a line without showing it, where the terminal can hide it.
pub fn ask_hidden(prompt: &str) -> Result<String, Stop> {
    if !raw::set(true) {
        println!("  (This terminal can't hide typing: the next line shows as you type it.)");
        return ask(prompt);
    }
    print!("  {prompt}");
    let _ = io::stdout().flush();
    let line = read_raw(&mut io::stdin().lock());
    raw::set(false);
    println!();
    line
}

fn read_line() -> Result<String, Stop> {
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line)?;
    if read == 0 {
        return Err(Stop::new("The terminal's input ended"));
    }
    Ok(line.trim().to_string())
}

/// A line typed in raw mode, where every key comes as it's pressed, with
/// no echo: up to Return, with Delete taking back the last character,
/// Control-U the whole line, and Control-C stopping.
fn read_raw(input: &mut impl Read) -> Result<String, Stop> {
    let mut typed = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if input.read(&mut byte)? == 0 {
            return Err(Stop::new("The terminal's input ended"));
        }
        match byte[0] {
            b'\r' | b'\n' => break,
            0x03 => return Err(Stop::new("Stopped")),
            0x04 if typed.is_empty() => return Err(Stop::new("The terminal's input ended")),
            0x7f | 0x08 => {
                // A character's bytes back to its first.
                while let Some(byte) = typed.pop() {
                    if byte & 0xc0 != 0x80 {
                        break;
                    }
                }
            }
            0x15 => typed.clear(),
            byte if byte >= 0x20 => typed.push(byte),
            _ => {}
        }
    }
    Ok(String::from_utf8_lossy(&typed).trim().to_string())
}

/// rootshell's terminal modes: cooked, the default, which echoes and gives
/// a line at a time, and raw (its wasm/README.md).
#[cfg(target_arch = "wasm32")]
mod raw {
    #[link(wasm_import_module = "rootshell_terminal")]
    unsafe extern "C" {
        fn rootshell_terminal_set_raw(enabled: i32) -> i32;
    }

    /// Whether the terminal took the mode.
    pub fn set(raw: bool) -> bool {
        unsafe { rootshell_terminal_set_raw(i32::from(raw)) == 0 }
    }
}

/// Off rootshell, which is only for the tests, typing isn't hidden.
#[cfg(not(target_arch = "wasm32"))]
mod raw {
    pub fn set(_raw: bool) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[u8]) -> Result<String, Stop> {
        read_raw(&mut io::Cursor::new(keys.to_vec()))
    }

    #[test]
    fn a_raw_line_ends_at_return_with_deletes_applied() {
        assert_eq!(typed(b"secret\r").unwrap(), "secret");
        assert_eq!(typed(b"secrex\x7ft\rmore").unwrap(), "secret");
        assert_eq!(
            typed("pässw\u{f6}\x7f\x7frd\n".as_bytes()).unwrap(),
            "pässrd"
        );
        assert_eq!(typed(b"wrong\x15right\r").unwrap(), "right");
        assert_eq!(typed(b"\x1b[Ahello\r").unwrap(), "[Ahello");
    }

    #[test]
    fn a_bar_fills_with_its_fraction() {
        assert_eq!(
            bar_line(46.0 / 67.0, "46 of 67 MB", None),
            "████████████░░░░░░  46 of 67 MB"
        );
        assert_eq!(bar_line(0.0, "0%", None), format!("{}  0%", "░".repeat(18)));
        assert_eq!(
            bar_line(1.0, "100%", None),
            format!("{}  100%", "█".repeat(18))
        );
        // Past the end, as a total that grew can make it, it stays full.
        assert_eq!(bar_line(1.5, "x", None), format!("{}  x", "█".repeat(18)));
        assert_eq!(
            bar_line(f64::NAN, "x", None),
            format!("{}  x", "░".repeat(18))
        );
    }

    #[test]
    fn a_bar_fits_the_terminal() {
        let width = |line: &str| 2 + line.chars().count();
        // An iPhone held upright.
        let line = bar_line(0.5, "460 of 670 MB", Some(36));
        assert!(width(&line) < 36, "{line}");
        assert!(line.starts_with("█████████░░░░░░░░░  "), "{line}");
        // A longer label takes cells from the bar.
        let line = bar_line(0.5, "1460 of 2670 MB", Some(36));
        assert!(width(&line) < 36, "{line}");
        assert!(line.starts_with("████████░░░░░░░░  "), "{line}");
        // Too narrow for a bar.
        assert_eq!(bar_line(0.5, "460 of 670 MB", Some(20)), "460 of 670 MB");
    }

    #[test]
    fn control_c_and_the_inputs_end_stop() {
        assert_eq!(typed(b"abc\x03def\r").unwrap_err().what, "Stopped");
        assert!(typed(b"abc").is_err());
        assert!(typed(b"\x04").is_err());
    }
}
