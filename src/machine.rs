//! Each anisette server's state, the machine Apple knows sign-ins through
//! it by, and the client info it was made as: the app's `AnisetteMachine`
//! (AppleIDKit), with its note in the same place, beside isideload's state.
//!
//! Apple makes a machine as the client info sent while it's made, and
//! answers a sign-in that sends another with -22410, "This action could not
//! be completed", until a new machine is made: other tools' users hit it
//! after Apple refused "macOS" on 20 Nov 2026 and they changed theirs, and
//! making the machine again fixed it (iloader #1304, SideStore #1745). So
//! the note says what the state was made as, and before a sign-in that
//! sends another, the state goes, for isideload to make a new machine. A
//! state with no note was made before the program kept them, as a client
//! info Apple refuses since then or the server's own, so it goes too.

use std::fs;
use std::io;
use std::path::Path;

use isideload::anisette::remote_v3::ANISETTE_STATE_KEY;

use crate::state::write_new;

/// The note's name, beside the state.
pub const NOTE: &str = "client_info";

#[derive(Debug, Clone, PartialEq)]
pub enum Readiness {
    /// Made as the client info the sign-in sends.
    Kept,
    /// None yet: the sign-in makes one.
    New,
    /// Made as another client info, or one not noted: deleted, so the
    /// sign-in makes a new one.
    Replaced(Option<String>),
}

/// The client info the state in `folder` was made as, as noted. None
/// without a state or a note.
pub fn made_as(folder: &Path) -> Option<String> {
    if !folder.join(ANISETTE_STATE_KEY).exists() {
        return None;
    }
    let note = fs::read_to_string(folder.join(NOTE)).ok()?;
    let note = note.trim();
    (!note.is_empty()).then(|| note.to_string())
}

/// Gets the state in `folder` ready for a sign-in that sends `client_info`:
/// kept when it was made as that, deleted otherwise, for the sign-in to make
/// a new one; and `client_info` noted, as what the state is or will be made
/// as.
pub fn ready(folder: &Path, client_info: &str) -> io::Result<Readiness> {
    let client_info = client_info.trim();
    let state = folder.join(ANISETTE_STATE_KEY);
    let readiness = if !state.exists() {
        Readiness::New
    } else {
        match made_as(folder) {
            Some(made) if made == client_info => return Ok(Readiness::Kept),
            made => {
                fs::remove_file(&state)?;
                Readiness::Replaced(made)
            }
        }
    };
    fs::create_dir_all(folder)?;
    write_new(&folder.join(NOTE), client_info.as_bytes())?;
    Ok(readiness)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC_OS_X: &str =
        "<MacBookPro18,3> <Mac OS X;26.6;25F84> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";
    const MAC_OS: &str =
        "<Mac15,7> <macOS;27.0;26A5378j> <com.apple.AuthKit/1 (com.apple.akd/1.0)>";

    /// isideload's state, as it writes it.
    fn make_state(folder: &Path) {
        fs::create_dir_all(folder).unwrap();
        fs::write(folder.join(ANISETTE_STATE_KEY), b"state").unwrap();
    }

    #[test]
    fn with_no_state_the_sign_in_makes_one_as_its_client_info() {
        let folder = crate::testing::scratch("machine-new")
            .join("anisette")
            .join("a.example");
        assert_eq!(ready(&folder, MAC_OS_X).unwrap(), Readiness::New);
        make_state(&folder);
        assert_eq!(made_as(&folder).as_deref(), Some(MAC_OS_X));
        assert_eq!(ready(&folder, MAC_OS_X).unwrap(), Readiness::Kept);
        assert!(folder.join(ANISETTE_STATE_KEY).exists());
        fs::remove_dir_all(folder.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_state_made_as_another_client_info_goes() {
        let folder = crate::testing::scratch("machine-other");
        make_state(&folder);
        write_new(&folder.join(NOTE), MAC_OS.as_bytes()).unwrap();
        assert_eq!(
            ready(&folder, MAC_OS_X).unwrap(),
            Readiness::Replaced(Some(MAC_OS.to_string()))
        );
        assert!(!folder.join(ANISETTE_STATE_KEY).exists());
        make_state(&folder);
        assert_eq!(made_as(&folder).as_deref(), Some(MAC_OS_X));
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_state_with_no_note_goes() {
        // As every run up to 0.4.0 left it.
        let folder = crate::testing::scratch("machine-unnoted");
        make_state(&folder);
        assert_eq!(made_as(&folder), None);
        assert_eq!(ready(&folder, MAC_OS_X).unwrap(), Readiness::Replaced(None));
        assert!(!folder.join(ANISETTE_STATE_KEY).exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_note_without_a_state_says_nothing() {
        let folder = crate::testing::scratch("machine-note-only");
        fs::create_dir_all(&folder).unwrap();
        write_new(&folder.join(NOTE), MAC_OS.as_bytes()).unwrap();
        assert_eq!(made_as(&folder), None);
        assert_eq!(ready(&folder, MAC_OS_X).unwrap(), Readiness::New);
        fs::remove_dir_all(folder).unwrap();
    }
}
