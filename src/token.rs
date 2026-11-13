//! The sign-in kept between runs, so that the next one goes without the
//! password and Apple's code while Apple takes it: Apple's token for its
//! developer services (`com.apple.gs.xcode.auth`), which the sign-in got
//! with the password, and the account it's for. Never the password, nor
//! Apple's session itself, which can ask for tokens for Apple's other
//! services: this token only does what this program does with Apple, until
//! Apple says it ends. It goes with the machine the sign-in was made as,
//! the anisette server and its state, which `.atelier` keeps too.
//!
//! In `.atelier/apple/<hash of the Apple ID>/sign-in.plist`, by the
//! signing key, so that neither file names the Apple ID. It goes with
//! `--forget`, once Apple refuses it, and some minutes before Apple says
//! it ends.
//!
//! An app that takes the sign-in in version 1 of the hand-off, such as
//! Atelier from build 1060, takes the password with it, so a run for one
//! still asks for the password and signs in with it. One that takes
//! version 2 takes the token in the password's place (certificate.rs):
//! the kept one, when Apple takes it.

use std::fs;
use std::io;
use std::path::PathBuf;

use isideload::auth::apple_account::AppToken;

use crate::state::{self, AppleIdHash, State};

/// How long before Apple's end for it that the token goes: a run takes
/// minutes, and shouldn't find it gone halfway.
const MARGIN: u64 = 15 * 60;

/// Not `Debug`, so that the token can't end up in the log.
#[derive(Clone, PartialEq)]
pub struct Token {
    /// The anisette server the sign-in went through: its state is the
    /// machine Apple knows the token by.
    pub server: String,
    /// What the server's machine said it was, as Atelier's list gave it:
    /// its client info and user agent.
    pub client_info: Option<(String, String)>,
    /// Apple's ID for the account, which goes with the token.
    pub adsid: String,
    pub token: String,
    /// As Apple gives them: how long it lasts, and when it ends.
    pub duration: u64,
    pub expiry: u64,
    /// When it was kept, in seconds since 1970.
    pub saved_at: u64,
}

impl Token {
    pub fn new(
        server: &str,
        client_info: Option<(String, String)>,
        adsid: &str,
        token: &AppToken,
        now: u64,
    ) -> Token {
        Token {
            server: server.to_string(),
            client_info,
            adsid: adsid.to_string(),
            token: token.token.clone(),
            duration: token.duration,
            expiry: token.expiry,
            saved_at: now,
        }
    }

    /// The token, as isideload's developer session takes it.
    pub fn app_token(&self) -> AppToken {
        AppToken {
            token: self.token.clone(),
            duration: self.duration,
            expiry: self.expiry,
        }
    }

    /// When Apple says it ends, in seconds since 1970. Apple doesn't say in
    /// what unit `expiry` is: as milliseconds, then as seconds, when it
    /// reads as a time from 2001 on; otherwise from when it was kept and
    /// how long it lasts, in seconds.
    pub fn ends_at(&self) -> Option<u64> {
        const MILLISECONDS: u64 = 1_000_000_000_000;
        const SECONDS: u64 = 1_000_000_000;
        if self.expiry >= MILLISECONDS {
            Some(self.expiry / 1000)
        } else if self.expiry >= SECONDS {
            Some(self.expiry)
        } else if self.duration > 0 {
            Some(self.saved_at.saturating_add(self.duration))
        } else {
            None
        }
    }

    /// Whether it's worth trying at `now`: Apple refuses one that has
    /// ended, and one whose end isn't known may still do.
    pub fn usable(&self, now: u64) -> bool {
        self.ends_at()
            .is_none_or(|end| now.saturating_add(MARGIN) < end)
    }

    /// The one kept for `account`, unless there's none, or it's too near
    /// its end to try, which this then deletes.
    pub fn load(state: &State, account: &AppleIdHash, now: u64) -> Option<Token> {
        let token = read(state, account)?;
        if token.usable(now) {
            Some(token)
        } else {
            Token::forget(state, account);
            None
        }
    }

    /// The Apple IDs a token worth trying is kept for, by their hashes, in
    /// no particular order.
    pub fn kept(state: &State, now: u64) -> Vec<AppleIdHash> {
        let Ok(folders) = fs::read_dir(state.apple()) else {
            return Vec::new();
        };
        folders
            .filter_map(|folder| AppleIdHash::from_name(folder.ok()?.file_name().to_str()?))
            .filter(|account| Token::load(state, account, now).is_some())
            .collect()
    }

    /// Keeps it for `account`, in place of the last one.
    pub fn save(&self, state: &State, account: &AppleIdHash) -> io::Result<()> {
        let text = |value: &str| plist::Value::String(value.to_string());
        let mut record = plist::Dictionary::new();
        record.insert("Server".into(), text(&self.server));
        if let Some((client_info, user_agent)) = &self.client_info {
            record.insert("ClientInfo".into(), text(client_info));
            record.insert("UserAgent".into(), text(user_agent));
        }
        record.insert("ADSID".into(), text(&self.adsid));
        record.insert("Token".into(), text(&self.token));
        record.insert(
            "Duration".into(),
            plist::Value::Integer(self.duration.into()),
        );
        record.insert("Expiry".into(), plist::Value::Integer(self.expiry.into()));
        record.insert(
            "SavedAt".into(),
            plist::Value::Integer(self.saved_at.into()),
        );
        let mut bytes = Vec::new();
        plist::Value::Dictionary(record)
            .to_writer_xml(&mut bytes)
            .map_err(io::Error::other)?;
        let path = path(state, account);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        state::write_new(&path, &bytes)
    }

    /// Deletes the one kept for `account`.
    pub fn forget(state: &State, account: &AppleIdHash) {
        let _ = fs::remove_file(path(state, account));
    }
}

/// Where the token for `account` is kept: by the signing key for it, which
/// is under the same hash (`certificate.rs`).
fn path(state: &State, account: &AppleIdHash) -> PathBuf {
    state.apple().join(account.as_str()).join("sign-in.plist")
}

fn read(state: &State, account: &AppleIdHash) -> Option<Token> {
    let bytes = fs::read(path(state, account)).ok()?;
    let record = plist::Value::from_reader(io::Cursor::new(bytes))
        .ok()?
        .into_dictionary()?;
    let text = |key: &str| {
        record
            .get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };
    let number = |key: &str| record.get(key).and_then(plist::Value::as_unsigned_integer);
    Some(Token {
        server: text("Server")?,
        client_info: text("ClientInfo").zip(text("UserAgent")),
        adsid: text("ADSID")?,
        token: text("Token")?,
        duration: number("Duration").unwrap_or(0),
        expiry: number("Expiry").unwrap_or(0),
        saved_at: number("SavedAt")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_794_500_000;

    fn token(expiry: u64, duration: u64) -> Token {
        Token {
            server: "https://ani.example".into(),
            client_info: Some((
                "<MacBookPro13,2> <macOS;13.1;22C65>".into(),
                "akd/1.0".into(),
            )),
            adsid: "000123-45-6789".into(),
            token: "a token".into(),
            duration,
            expiry,
            saved_at: NOW,
        }
    }

    #[test]
    fn reads_apples_end_in_either_unit() {
        let day = 24 * 60 * 60;
        assert_eq!(token((NOW + day) * 1000, 0).ends_at(), Some(NOW + day));
        assert_eq!(token(NOW + day, 0).ends_at(), Some(NOW + day));
        assert_eq!(token(0, day).ends_at(), Some(NOW + day));
        assert_eq!(token(0, 0).ends_at(), None);
    }

    #[test]
    fn tries_it_until_shortly_before_its_end() {
        assert!(token(NOW + 3600, 0).usable(NOW));
        assert!(!token(NOW + MARGIN, 0).usable(NOW));
        assert!(!token(NOW - 1, 0).usable(NOW));
        // An end that isn't known is Apple's to say.
        assert!(token(0, 0).usable(NOW));
    }

    #[test]
    fn keeps_one_for_each_apple_id_and_lets_an_old_one_go() {
        let folder = crate::testing::scratch("token");
        let state = State::at(folder.clone());
        let (a, b) = (
            AppleIdHash::of("a@example.com"),
            AppleIdHash::of("b@example.com"),
        );
        assert!(Token::load(&state, &a, NOW).is_none());

        let kept = token((NOW + 3600) * 1000, 3600);
        kept.save(&state, &a).unwrap();
        assert!(Token::load(&state, &a, NOW) == Some(kept.clone()));
        assert!(Token::load(&state, &b, NOW).is_none());
        // Neither the Apple ID nor anything of it in the clear.
        let written = fs::read_to_string(path(&state, &a)).unwrap();
        assert!(!written.contains("example.com"));

        let mut without = kept.clone();
        without.client_info = None;
        without.save(&state, &a).unwrap();
        assert!(Token::load(&state, &a, NOW) == Some(without));

        // Too near its end: it goes.
        assert!(Token::load(&state, &a, NOW + 3600 - 60).is_none());
        assert!(!path(&state, &a).exists());

        kept.save(&state, &a).unwrap();
        Token::forget(&state, &a);
        assert!(Token::load(&state, &a, NOW).is_none());

        fs::write(path(&state, &a), b"not a plist").unwrap();
        assert!(Token::load(&state, &a, NOW).is_none());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn finds_the_apple_ids_a_token_is_kept_for() {
        let folder = crate::testing::scratch("token-kept");
        let state = State::at(folder.clone());
        assert!(Token::kept(&state, NOW).is_empty());
        let (a, b) = (
            AppleIdHash::of("a@example.com"),
            AppleIdHash::of("b@example.com"),
        );
        token((NOW + 3600) * 1000, 3600).save(&state, &a).unwrap();
        // A key alone, a token near its end, and a folder of another name.
        fs::create_dir_all(state.apple().join(b.as_str())).unwrap();
        fs::write(state.apple().join(b.as_str()).join("key"), b"key").unwrap();
        let c = AppleIdHash::of("c@example.com");
        token(NOW + 60, 0).save(&state, &c).unwrap();
        fs::create_dir_all(state.apple().join("not-a-hash")).unwrap();
        assert_eq!(Token::kept(&state, NOW), std::slice::from_ref(&a));

        token((NOW + 3600) * 1000, 3600).save(&state, &b).unwrap();
        let mut both = Token::kept(&state, NOW);
        both.sort_by(|x, y| x.as_str().cmp(y.as_str()));
        let mut expected = vec![a, b];
        expected.sort_by(|x, y| x.as_str().cmp(y.as_str()));
        assert_eq!(both, expected);
        fs::remove_dir_all(folder).unwrap();
    }
}
