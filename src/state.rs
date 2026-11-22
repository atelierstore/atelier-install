//! What the program keeps between runs, in `.atelier` in rootshell's folder,
//! which Files doesn't show, and the folder Files does show as rootshell's.
//!
//! | What | Where | Until |
//! |---|---|---|
//! | The pairing file | `pairing.plist` | the app takes it, or `--forget` |
//! | The pairing service's port that worked last | `port` | `--forget` |
//! | The signing key Apple made a certificate for | `apple/<hash of the Apple ID>/key` | `--forget` |
//! | Apple's token for the sign-in, not the password | `apple/<hash of the Apple ID>/sign-in.plist` | Apple refuses it, it nears its end, or `--forget` (token.rs) |
//! | Each anisette server's state, and the one that worked last | `anisette/` | `--forget` |
//! | What the sign-in waits for, and what stopped runs waited for | `anisette/trying`, `anisette/stalled` | a sign-in goes through |
//! | The IPA, unpacked and signed | `work/` | the install's done (kept.rs) |
//! | A downloaded IPA, and its link | `download.ipa`, `download.link` | the install's done |
//! | What was signed, and what the app takes | `install.plist` | the install's done (kept.rs) |
//! | The certificate and the sign-in the app takes | `install-signing.plist` | the app has them (kept.rs) |
//!
//! Each file is written whole or not at all (`write_new`), so that a run
//! stopped at any moment leaves what it kept for the next one to go on
//! from.
//!
//! Apple's codes are never written here, nor is the Apple ID's password,
//! but for an app that takes the sign-in, such as Atelier: from the signing
//! to the hand-off, so that a run stopped before it was installed goes on
//! from there. Then it's in the app's own container alone (certificate.rs).
//! `--redo` deletes it too. What's kept of the sign-in otherwise is Apple's
//! token for its developer services, so that the next run can go without
//! the password (token.rs).
//!
//! rootshell's programs only see its folder: `/` is the folder itself, and
//! `HOME` is `/`. A program's files there are no one else's: rootshell is the
//! only app that reaches them, as Files' other apps don't.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct State {
    folder: PathBuf,
}

impl State {
    /// `$ATELIER_HOME`, or `.atelier` in the home folder: rootshell's.
    pub fn open() -> io::Result<State> {
        let folder = match std::env::var_os("ATELIER_HOME") {
            Some(folder) => PathBuf::from(folder),
            None => files_folder().join(".atelier"),
        };
        State::open_in(folder)
    }

    fn open_in(folder: PathBuf) -> io::Result<State> {
        fs::create_dir_all(&folder)?;
        finish_writes(&folder, &folder.join("work"));
        Ok(State { folder })
    }

    #[cfg(test)]
    pub fn at(folder: PathBuf) -> State {
        State { folder }
    }

    pub fn pairing(&self) -> PathBuf {
        self.folder.join("pairing.plist")
    }

    /// The pairing service's port, when one worked before.
    pub fn port(&self) -> Option<u16> {
        fs::read_to_string(self.folder.join("port"))
            .ok()?
            .trim()
            .parse()
            .ok()
            .filter(|port| *port != 0)
    }

    pub fn set_port(&self, port: u16) {
        let _ = write_new(&self.folder.join("port"), port.to_string().as_bytes());
    }

    /// isideload's storage: the signing key.
    pub fn apple(&self) -> PathBuf {
        self.folder.join("apple")
    }

    /// One anisette server's state, so switching servers never mixes
    /// their states, as the app keeps them.
    pub fn anisette(&self, url: &str) -> PathBuf {
        let host: String = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.folder.join("anisette").join(host)
    }

    /// The anisette server the last sign-in worked through.
    pub fn last_server(&self) -> Option<String> {
        let text = fs::read_to_string(self.folder.join("anisette").join("last")).ok()?;
        Some(text.trim().to_string()).filter(|url| !url.is_empty())
    }

    pub fn set_last_server(&self, url: &str) {
        let folder = self.folder.join("anisette");
        let _ = fs::create_dir_all(&folder);
        let _ = write_new(&folder.join("last"), url.as_bytes());
    }

    /// Notes that the sign-in is waiting for `url`. rootshell's
    /// connections wait for a server for as long as it takes, so a run
    /// stopped there, with Control-C, leaves the note behind.
    pub fn set_trying(&self, url: &str) {
        let folder = self.folder.join("anisette");
        let _ = fs::create_dir_all(&folder);
        let _ = write_new(&folder.join("trying"), url.as_bytes());
    }

    /// Notes that what the sign-in waited for has answered.
    pub fn done_trying(&self) {
        let _ = fs::remove_file(self.folder.join("anisette").join("trying"));
    }

    /// What runs were stopped waiting for, the last one's last, and the
    /// last one's alone when the last run was stopped so. None of them
    /// counts as the server that worked last any more.
    pub fn take_stalled(&self) -> (Vec<String>, Option<String>) {
        let folder = self.folder.join("anisette");
        let mut stalled: Vec<String> = fs::read_to_string(folder.join("stalled"))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .collect();
        let last = fs::read_to_string(folder.join("trying"))
            .ok()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty());
        if let Some(url) = &last {
            stalled.retain(|stalled| stalled != url);
            stalled.push(url.clone());
            let _ = write_new(&folder.join("stalled"), stalled.join("\n").as_bytes());
            self.done_trying();
        }
        if self.last_server().is_some_and(|url| stalled.contains(&url)) {
            let _ = fs::remove_file(folder.join("last"));
        }
        (stalled, last)
    }

    /// Whether a run was stopped waiting for `url`: the last one, or one
    /// before it, since a sign-in last went through.
    pub fn stalled_at(&self, url: &str) -> bool {
        let folder = self.folder.join("anisette");
        let trying = fs::read_to_string(folder.join("trying")).unwrap_or_default();
        let stalled = fs::read_to_string(folder.join("stalled")).unwrap_or_default();
        trying.trim() == url || stalled.lines().any(|line| line.trim() == url)
    }

    /// Forgets what runs were stopped waiting for, once a sign-in has gone
    /// through.
    pub fn clear_stalled(&self) {
        let _ = fs::remove_file(self.folder.join("anisette").join("stalled"));
    }

    /// Where isideload unpacks and signs the IPA: its temporary folder.
    pub fn work(&self) -> PathBuf {
        self.folder.join("work")
    }

    pub fn download(&self) -> PathBuf {
        self.folder.join("download.ipa")
    }

    /// The link the downloaded IPA came from.
    pub fn download_link(&self) -> PathBuf {
        self.folder.join("download.link")
    }

    /// The kept install (kept.rs).
    pub fn install(&self) -> PathBuf {
        self.folder.join("install.plist")
    }

    /// What the kept install's app takes: the certificate and the sign-in.
    pub fn install_signing(&self) -> PathBuf {
        self.folder.join("install-signing.plist")
    }

    /// Deletes the downloaded IPA.
    pub fn forget_download(&self) {
        let _ = fs::remove_file(self.download());
        let _ = fs::remove_file(self.download_link());
    }

    /// Deletes everything kept.
    pub fn forget(&self) -> io::Result<()> {
        fs::remove_dir_all(&self.folder)
    }
}

/// The folder Files shows as rootshell's: `HOME`, which rootshell sets to
/// it.
pub fn files_folder() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Writes a kept file anew, whole or not at all, so that a run stopped at
/// any moment, with Control-C or by iOS, leaves the last copy or the new
/// one, never part of one. The new copy goes to `<name>.writing`, a fresh
/// file, so none of an earlier one is left in it; once it's all there, to
/// `<name>.written`; then over the file. rootshell renames onto a file by
/// deleting it first (paths.rs), so a run stopped between the two leaves
/// `<name>.written` alone, whole, and the next run's `State::open` puts it
/// in place (`finish_writes`).
pub fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let writing = beside(path, WRITING);
    let written = beside(path, WRITTEN);
    let _ = fs::remove_file(&writing);
    fs::write(&writing, bytes)?;
    fs::rename(&writing, &written)?;
    fs::rename(&written, path)
}

/// The ends of `write_new`'s copies, while one is written and once all of
/// it is.
const WRITING: &str = ".writing";
const WRITTEN: &str = ".written";

fn beside(path: &Path, end: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(end);
    path.with_file_name(name)
}

/// What a stopped run's `write_new` left: a copy written whole goes in
/// place, one it hadn't finished goes. `skip` is the work folder, which
/// holds the signed app's many files and none of these.
fn finish_writes(folder: &Path, skip: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            if path != skip {
                finish_writes(&path, skip);
            }
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(kept) = name.strip_suffix(WRITTEN) {
            let kept = path.with_file_name(kept);
            let _ = fs::rename(&path, kept);
        } else if name.ends_with(WRITING) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// What an Apple ID's sign-in and signing key are kept under: a hash of
/// it, so that no file names it. A run that reuses the one kept sign-in
/// finds it by this alone, without the Apple ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppleIdHash(String);

impl AppleIdHash {
    pub fn of(apple_id: &str) -> AppleIdHash {
        AppleIdHash(hex::encode(Sha256::digest(apple_id.as_bytes())))
    }

    /// A folder's name that's one, as `of` makes them.
    pub fn from_name(name: &str) -> Option<AppleIdHash> {
        (name.len() == 64
            && name
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
        .then(|| AppleIdHash(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_stopped_waiting_leaves_its_server_for_last() {
        let folder = crate::testing::scratch("stalled");
        let state = State::at(folder.clone());
        assert_eq!(state.take_stalled(), (vec![], None));

        state.set_last_server("https://b");
        state.set_trying("https://a");
        state.done_trying();
        assert_eq!(state.take_stalled(), (vec![], None));

        state.set_trying("https://b");
        assert!(state.stalled_at("https://b") && !state.stalled_at("https://a"));
        assert_eq!(
            state.take_stalled(),
            (vec!["https://b".to_string()], Some("https://b".to_string()))
        );
        assert!(state.stalled_at("https://b"));
        assert_eq!(state.last_server(), None);
        state.set_trying("https://c");
        state.set_trying("https://a");
        assert_eq!(
            state.take_stalled(),
            (
                vec!["https://b".to_string(), "https://a".to_string()],
                Some("https://a".to_string())
            )
        );
        assert_eq!(
            state.take_stalled(),
            (vec!["https://b".to_string(), "https://a".to_string()], None)
        );
        state.clear_stalled();
        assert_eq!(state.take_stalled(), (vec![], None));
        assert!(!state.stalled_at("https://b"));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn each_anisette_server_has_its_own_folder() {
        let state = State::at(PathBuf::from("/x"));
        assert_eq!(
            state.anisette("https://ani.sidestore.io"),
            PathBuf::from("/x/anisette/ani.sidestore.io")
        );
        assert_eq!(
            state.anisette("https://example.com:8443/v3?x=1"),
            PathBuf::from("/x/anisette/example.com_8443_v3_x_1")
        );
    }

    #[test]
    fn keeps_the_port_and_the_last_server() {
        let folder = crate::testing::scratch("state");
        let state = State::at(folder.clone());
        assert_eq!(state.port(), None);
        state.set_port(49153);
        assert_eq!(state.port(), Some(49153));
        assert_eq!(state.last_server(), None);
        state.set_last_server("https://a.example");
        assert_eq!(state.last_server().as_deref(), Some("https://a.example"));
        write_new(&state.pairing(), b"one, the longer").unwrap();
        write_new(&state.pairing(), b"two").unwrap();
        assert_eq!(fs::read(state.pairing()).unwrap(), b"two");
        state.forget().unwrap();
        assert!(!folder.exists());
    }

    #[test]
    fn writes_leave_nothing_beside_the_file() {
        let folder = crate::testing::scratch("write-new");
        let file = folder.join("pairing.plist");
        write_new(&file, b"one").unwrap();
        write_new(&file, b"two").unwrap();
        let mut names: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["pairing.plist"]);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn the_next_run_finishes_what_a_stopped_one_wrote() {
        let folder = crate::testing::scratch("finish-writes");
        let anisette = folder.join("anisette").join("ani.example");
        let work = folder.join("work");
        fs::create_dir_all(&anisette).unwrap();
        fs::create_dir_all(&work).unwrap();
        // Stopped once the new copy was whole: before the old one went,
        // and after.
        fs::write(folder.join("pairing.plist"), "old").unwrap();
        fs::write(folder.join("pairing.plist.written"), "new").unwrap();
        fs::write(anisette.join("state.written"), "new").unwrap();
        // Stopped while writing: the last copy stays.
        fs::write(folder.join("port"), "49152").unwrap();
        fs::write(folder.join("port.writing"), "491").unwrap();
        // The signed app's own, left as they are.
        fs::write(work.join("Info.plist.written"), "app's").unwrap();

        let state = State::open_in(folder.clone()).unwrap();
        assert_eq!(fs::read_to_string(state.pairing()).unwrap(), "new");
        assert!(!folder.join("pairing.plist.written").exists());
        assert_eq!(fs::read_to_string(anisette.join("state")).unwrap(), "new");
        assert_eq!(state.port(), Some(49152));
        assert!(!folder.join("port.writing").exists());
        assert!(work.join("Info.plist.written").exists());
        fs::remove_dir_all(folder).unwrap();
    }
}
