//! An install, kept from its signing until it's done, so that a run
//! stopped on the way, by a connection that broke or Control-C, goes on
//! from there the next time: no download, no sign-in and no signing again.
//! In `.atelier`:
//!
//! - `install.plist`: what was signed, from which IPA, and what the app
//!   takes (handoff.rs);
//! - `install-signing.plist`: the certificate and the sign-in the app
//!   takes, its password or Apple's token included, when it takes them
//!   (certificate.rs);
//! - the signed app, where it was signed, in `work/`.
//!
//! It all goes once the app is installed and has what it takes, when
//! another IPA is signed, with `--redo` or `--forget`, and a day after the
//! signing, as the app then has less of its 7 days left.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::certificate::Format;
use crate::handoff::HandOffs;
use crate::state::{self, State};

/// How long a signed app waits for its install.
const KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    /// The IPA's link, or its path, as given; empty for the one in
    /// rootshell's folder.
    pub source: String,
    pub sha256: String,
    /// The IPA's bundle ID, and the one it installs as, with the team ID.
    pub ipa_bundle_id: String,
    pub bundle_id: String,
    pub name: String,
    pub version: String,
    /// The signed app's folder.
    pub app: PathBuf,
    /// What the app takes, as the user agreed to it.
    pub hand_offs: HandOffs,
    /// Why the app doesn't get the sign-in it takes, when that's so.
    pub signing_problem: Option<String>,
    /// When it was signed, in seconds since 1970.
    pub signed_at: u64,
}

impl Install {
    /// The last run's install, unless there's none, or it's too old to go
    /// on with, or its signed app is gone, which this then deletes.
    pub fn load(state: &State) -> Option<Install> {
        let install = read(state);
        match install {
            Some(install)
                if now().saturating_sub(install.signed_at) < KEPT_FOR.as_secs()
                    && install.app.is_dir() =>
            {
                Some(install)
            }
            _ => {
                remove(state, install.as_ref());
                None
            }
        }
    }

    /// Keeps it, and the sign-in the app takes, if any.
    pub fn save(&self, state: &State, signing: Option<&[u8]>) -> io::Result<()> {
        let text = |value: &str| plist::Value::String(value.to_string());
        let mut record = plist::Dictionary::new();
        record.insert("Source".into(), text(&self.source));
        record.insert("SHA256".into(), text(&self.sha256));
        record.insert("IPABundleID".into(), text(&self.ipa_bundle_id));
        record.insert("BundleID".into(), text(&self.bundle_id));
        record.insert("Name".into(), text(&self.name));
        record.insert("Version".into(), text(&self.version));
        record.insert("App".into(), text(&self.app.to_string_lossy()));
        if let Some(path) = &self.hand_offs.pairing {
            record.insert("PairingTo".into(), text(path));
        }
        if let Some(path) = &self.hand_offs.signing {
            record.insert("SigningTo".into(), text(path));
            record.insert(
                "SigningVersion".into(),
                plist::Value::Integer(self.hand_offs.format.version().into()),
            );
        }
        if let Some(problem) = &self.signing_problem {
            record.insert("SigningProblem".into(), text(problem));
        }
        record.insert(
            "SignedAt".into(),
            plist::Value::Integer(self.signed_at.into()),
        );
        let mut bytes = Vec::new();
        plist::Value::Dictionary(record)
            .to_writer_xml(&mut bytes)
            .map_err(io::Error::other)?;
        let _ = fs::remove_file(state.install_signing());
        let saved = match signing {
            Some(signing) => state::write_new(&state.install_signing(), signing),
            None => Ok(()),
        }
        .and_then(|()| state::write_new(&state.install(), &bytes));
        if saved.is_err() {
            // No sign-in is left without the record it goes with.
            remove(state, None);
        }
        saved
    }

    /// The certificate and the sign-in the app takes, as kept.
    pub fn signing(&self, state: &State) -> Option<Vec<u8>> {
        self.hand_offs.signing.as_ref()?;
        fs::read(state.install_signing()).ok()
    }

    /// Whether a run that asks for the IPA at `link`, of the SHA-256
    /// `sha256` when it's given, asks for this one.
    pub fn is_from(&self, link: &str, sha256: Option<&str>) -> bool {
        self.source == link
            && sha256.is_none_or(|sha256| sha256.trim().eq_ignore_ascii_case(&self.sha256))
    }

    /// Deletes it all: the record, the sign-in and the signed app.
    pub fn remove(&self, state: &State) {
        remove(state, Some(self));
    }
}

/// Deletes the kept install, `install` when it could be read.
pub fn remove(state: &State, install: Option<&Install>) {
    if let Some(install) = install {
        // <work folder>/<IPA>_extracted/Payload/<name>.app; nothing out of
        // the work folder, whatever the record says.
        if let Some(unpacked) = install.app.ancestors().nth(2)
            && unpacked.starts_with(state.work())
            && unpacked != state.work()
        {
            let _ = fs::remove_dir_all(unpacked);
        }
    }
    let _ = fs::remove_file(state.install_signing());
    let _ = fs::remove_file(state.install());
}

/// At the end of a run: deletes the work folder, where the IPA was unpacked
/// and signed, unless an install waits in it for the next run.
pub fn tidy(state: &State) {
    if Install::load(state).is_none() {
        let _ = fs::remove_dir_all(state.work());
    }
}

fn read(state: &State) -> Option<Install> {
    let bytes = fs::read(state.install()).ok()?;
    let record = plist::Value::from_reader(io::Cursor::new(bytes))
        .ok()?
        .into_dictionary()?;
    let text = |key: &str| {
        record
            .get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };
    Some(Install {
        source: text("Source")?,
        sha256: text("SHA256")?,
        ipa_bundle_id: text("IPABundleID")?,
        bundle_id: text("BundleID")?,
        name: text("Name")?,
        version: text("Version").unwrap_or_default(),
        app: PathBuf::from(text("App")?),
        hand_offs: HandOffs {
            pairing: text("PairingTo"),
            signing: text("SigningTo"),
            // As this program's builds kept it before version 2.
            format: match record.get("SigningVersion") {
                None => Format::Password,
                Some(version) => Format::of_version(version.as_signed_integer()?)?,
            },
        },
        signing_problem: text("SigningProblem"),
        signed_at: record
            .get("SignedAt")
            .and_then(plist::Value::as_unsigned_integer)?,
    })
}

/// Seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(state: &State) -> Install {
        let app = state
            .work()
            .join("Atelier.ipa_extracted/Payload/Atelier.app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("Atelier"), b"signed").unwrap();
        Install {
            source: "https://example.com/Atelier.ipa".into(),
            sha256: "ab".repeat(32),
            ipa_bundle_id: "com.atelier.app".into(),
            bundle_id: "com.atelier.app.TEAM".into(),
            name: "Atelier".into(),
            version: "0.6.0".into(),
            app,
            hand_offs: HandOffs {
                pairing: Some("Library/Application Support/Pairing/pairingFile.plist".into()),
                signing: Some("Library/Caches/Installer/certificate.plist".into()),
                format: Format::Token,
            },
            signing_problem: None,
            signed_at: now(),
        }
    }

    #[test]
    fn keeps_an_install_until_its_done() {
        let folder = crate::testing::scratch("kept");
        let state = State::at(folder.clone());
        assert_eq!(Install::load(&state), None);

        let kept = install(&state);
        kept.save(&state, Some(b"sign-in")).unwrap();
        let loaded = Install::load(&state).unwrap();
        assert_eq!(loaded, kept);
        assert_eq!(loaded.signing(&state).as_deref(), Some(&b"sign-in"[..]));
        assert!(loaded.is_from("https://example.com/Atelier.ipa", None));
        assert!(loaded.is_from("https://example.com/Atelier.ipa", Some(&"AB".repeat(32))));
        assert!(!loaded.is_from("https://example.com/Atelier.ipa", Some(&"cd".repeat(32))));
        assert!(!loaded.is_from("https://example.com/Other.ipa", None));

        // One kept by this program's builds before version 2 is version 1.
        let mut record = plist::Value::from_file(state.install())
            .unwrap()
            .into_dictionary()
            .unwrap();
        record.remove("SigningVersion");
        plist::Value::Dictionary(record)
            .to_file_xml(state.install())
            .unwrap();
        assert_eq!(
            Install::load(&state).unwrap().hand_offs.format,
            Format::Password
        );

        // Saved again without a sign-in, none is left.
        let mut without = kept.clone();
        without.hand_offs.signing = None;
        without.hand_offs.format = Format::Password;
        without.save(&state, None).unwrap();
        assert!(!state.install_signing().exists());
        assert_eq!(Install::load(&state).unwrap().signing(&state), None);

        kept.save(&state, Some(b"sign-in")).unwrap();
        Install::load(&state).unwrap().remove(&state);
        assert_eq!(Install::load(&state), None);
        assert!(!state.install_signing().exists());
        assert!(!state.work().join("Atelier.ipa_extracted").exists());
        assert!(state.work().exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn lets_one_whose_app_is_gone_go() {
        let folder = crate::testing::scratch("kept-gone");
        let state = State::at(folder.clone());
        let kept = install(&state);
        kept.save(&state, Some(b"sign-in")).unwrap();
        fs::remove_dir_all(state.work()).unwrap();
        assert_eq!(Install::load(&state), None);
        assert!(!state.install().exists() && !state.install_signing().exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn lets_an_old_one_go() {
        let folder = crate::testing::scratch("kept-old");
        let state = State::at(folder.clone());
        let mut old = install(&state);
        old.signed_at = now() - KEPT_FOR.as_secs() - 1;
        old.save(&state, Some(b"sign-in")).unwrap();
        assert_eq!(Install::load(&state), None);
        assert!(!state.install().exists() && !state.install_signing().exists());
        assert!(!state.work().join("Atelier.ipa_extracted").exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn a_run_leaves_the_work_folder_to_an_install_that_waits() {
        let folder = crate::testing::scratch("kept-tidy");
        let state = State::at(folder.clone());
        let kept = install(&state);
        kept.save(&state, Some(b"sign-in")).unwrap();
        tidy(&state);
        assert!(kept.app.join("Atelier").is_file());
        assert_eq!(Install::load(&state), Some(kept.clone()));

        kept.remove(&state);
        tidy(&state);
        assert!(!state.work().exists());

        // Nor is a sign-in left without its record.
        fs::create_dir_all(state.work()).unwrap();
        state::write_new(&state.install_signing(), b"sign-in").unwrap();
        tidy(&state);
        assert!(!state.install_signing().exists() && !state.work().exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn deletes_nothing_out_of_the_work_folder() {
        let folder = crate::testing::scratch("kept-outside");
        let state = State::at(folder.clone());
        let outside = folder.join("elsewhere/Payload/App.app");
        fs::create_dir_all(&outside).unwrap();
        let mut kept = install(&state);
        kept.app = outside.clone();
        kept.save(&state, None).unwrap();
        Install::load(&state).unwrap().remove(&state);
        assert!(outside.exists());
        fs::remove_dir_all(folder).unwrap();
    }
}
