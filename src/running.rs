//! The run that's going with the page, so that a new run can ask it to
//! stop and go on from where it was.
//!
//! You can run the Shortcut again while its last run still goes in
//! rootshell: once its web view went with rootshell in the background, say.
//! rootshell then holds a new tab's curl and ifconfig until the program in
//! the other tab ends, though a second program runs (Alex's tests, 22 Nov
//! 2026). So the Shortcut's line first asks that run to stop, with the
//! program already in rootshell's folder, and only when one may be going:
//! `[ -f .atelier/running ] && ./atelier-install.wasm --step-aside;`, as
//! `[` is the shell's own. A run with the page asks as well as it starts,
//! for a line without that part.
//!
//! `.atelier/running` holds the page's port and a word of the run's own,
//! not the page's key. Asked at `/step-aside` with that word, the run says
//! why in its tab, deletes the file and ends, as Control-C would: what it
//! kept is the new run's to go on from. The file goes as a run ends; one
//! that a stopped run left (Control-C, or rootshell quit) names a port where
//! nothing answers, or a word nobody knows there, and goes too.

use std::fs;
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::net::Stream;
use crate::state::{self, State};
use crate::terminal;

/// The header the word comes in.
pub const HEADER: &str = "x-atelier-step-aside";
/// How long the run going has to answer: it does between two of its own
/// steps' moments, as it answers the page.
const ANSWER: Duration = Duration::from_secs(10);
/// How long it has to end, once it said it would.
const END: Duration = Duration::from_secs(5);
/// A moment more, for rootshell to see its program ended.
const ENDED: Duration = Duration::from_millis(500);

/// This run's file and word, once it noted them.
static MINE: OnceLock<(PathBuf, String)> = OnceLock::new();

/// Notes this run as the one going, with the page at `port`: the word a
/// new run asks it to stop with.
pub fn note(state: &State, port: u16) -> Option<String> {
    let mut word = [0u8; 16];
    getrandom::getrandom(&mut word).ok()?;
    let word = hex::encode(word);
    let path = state.running();
    if let Err(error) = state::write_new(&path, format!("{port} {word}\n").as_bytes()) {
        terminal::log(format!("noting the run: {error}"));
        return None;
    }
    let _ = MINE.set((path, word.clone()));
    Some(word)
}

/// As this run ends: no longer the one going.
pub fn clear() {
    if let Some((path, word)) = MINE.get()
        && read(path).is_some_and(|(_, noted)| noted == *word)
    {
        let _ = fs::remove_file(path);
    }
}

fn read(path: &Path) -> Option<(u16, String)> {
    let text = fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let port = parts.next()?.parse().ok().filter(|port| *port != 0)?;
    let word = parts.next()?.to_string();
    Some((port, word))
}

/// Asks the run going, if one is, to stop, and waits for it to end. Says
/// so when one did.
pub fn step_aside(state: &State) -> bool {
    let path = state.running();
    let Some((port, word)) = read(&path) else {
        let _ = fs::remove_file(&path);
        return false;
    };
    match ask(port, &word) {
        Ok(true) => {}
        Ok(false) | Err(_) => {
            // A run that was stopped left it, or another program has the
            // port now.
            let _ = fs::remove_file(&path);
            return false;
        }
    }
    let asked = Instant::now();
    while path.exists() && asked.elapsed() < END {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(ENDED);
    let _ = fs::remove_file(&path);
    println!("The last run was still going in another tab. It has stopped, and this");
    println!("one goes on from where it was.");
    true
}

/// Whether the run at `port` took `word` and is stopping.
fn ask(port: u16, word: &str) -> std::io::Result<bool> {
    let mut stream = Stream::connect(Ipv4Addr::LOCALHOST, port)?;
    stream.set_read_limit(Some(ANSWER))?;
    let request = format!(
        "POST /step-aside HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{HEADER}: {word}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes())?;
    let mut answer = Vec::new();
    let mut chunk = [0u8; 512];
    while !answer.windows(4).any(|end| end == b"\r\n\r\n") && answer.len() < 4096 {
        match stream.read(&mut chunk)? {
            0 => break,
            read => answer.extend_from_slice(&chunk[..read]),
        }
    }
    Ok(answer.starts_with(b"HTTP/1.1 200 "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::door::tests::opened;

    #[test]
    fn notes_this_run_and_clears_only_its_own_note() {
        let state = State::at(crate::testing::scratch("running"));
        let word = note(&state, 24054).unwrap();
        assert_eq!(word.len(), 32);
        assert_eq!(read(&state.running()), Some((24054, word.clone())));
        // Another run's note now: not this run's to clear.
        fs::write(state.running(), "24055 another\n").unwrap();
        clear();
        assert!(state.running().exists());
        fs::write(state.running(), format!("24054 {word}\n")).unwrap();
        clear();
        assert!(!state.running().exists());
    }

    #[test]
    fn a_note_nobody_answers_for_goes_and_nothing_is_said() {
        let state = State::at(crate::testing::scratch("stale"));
        assert!(!step_aside(&state));
        // A run that was stopped left it: nothing answers at its port.
        let (door, port) = opened();
        drop(door);
        fs::write(state.running(), format!("{port} 0123456789abcdef\n")).unwrap();
        assert!(!step_aside(&state));
        assert!(!state.running().exists());
        for unreadable in ["", "0 word\n", "24054\n", "port word\n"] {
            fs::write(state.running(), unreadable).unwrap();
            assert!(!step_aside(&state));
            assert!(!state.running().exists());
        }
    }
}
