//! Step 7, for any app: what an app takes from this program once it's
//! installed, and where in its container. The app says so in its
//! Info.plist ([`DECLARATION`]), and this program writes what it declares,
//! at the paths it declares, and nothing else. Nothing that comes with an
//! IPA runs here: running it in the program that holds the pairing and the
//! password would be the dangerous kind of post-processing. An app that
//! declares nothing gets nothing, and any app but Atelier gets something
//! only once the user said yes to it.
//!
//! Atelier's builds from before the declaration read the pairing at
//! [`ATELIER_PAIRING`], and the certificate and the sign-in at
//! `certificate::PATH` when they say `AtelierInstallerHandOff`
//! (`certificate.rs`), so Atelier takes those unless it
//! declares others.

use crate::certificate;
use crate::ipa::ATELIER;

/// The key in an app's Info.plist that says what it takes:
///
/// ```xml
/// <key>SideloadHandOff</key>
/// <dict>
///     <key>Version</key>
///     <integer>1</integer>
///     <key>PairingFile</key>
///     <string>Documents/pairingFile.plist</string>
///     <key>SigningIdentity</key>
///     <string>Library/Caches/Installer/certificate.plist</string>
/// </dict>
/// ```
///
/// Either path can be left out. In version 1, the pairing is an RPPairing
/// file as idevice writes it, and the signing identity is certificate.rs's
/// format, its version 1.
pub const DECLARATION: &str = "SideloadHandOff";
pub const VERSION: i64 = 1;
const PAIRING_FILE: &str = "PairingFile";
const SIGNING_IDENTITY: &str = "SigningIdentity";

/// Where Atelier reads its pairing, in its own container.
pub const ATELIER_PAIRING: &str = "Library/Application Support/Pairing/pairingFile.plist";

/// The folders of an app's container a hand-off can go in.
const TOP_FOLDERS: [&str; 3] = ["Documents", "Library", "tmp"];

/// What an app takes, at paths from its container's root.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HandOffs {
    /// This iPhone's pairing.
    pub pairing: Option<String>,
    /// The certificate this program signed with, its key and the sign-in:
    /// the Apple ID and password, the iPhone, and the anisette server and
    /// state the sign-in went through (certificate.rs).
    pub signing: Option<String>,
}

impl HandOffs {
    pub fn is_empty(&self) -> bool {
        self.pairing.is_none() && self.signing.is_none()
    }

    /// What the app `bundle_id`, whose Info.plist is `info`, takes, and
    /// why what it declares isn't taken, when that's so.
    pub fn of(bundle_id: &str, info: &plist::Dictionary) -> (HandOffs, Option<String>) {
        let (declared, refused) = match declared(info) {
            Ok(declared) => (declared, None),
            Err(why) => (HandOffs::default(), Some(why)),
        };
        if bundle_id != ATELIER {
            return (declared, refused);
        }
        let atelier = HandOffs {
            pairing: declared
                .pairing
                .or_else(|| Some(ATELIER_PAIRING.to_string())),
            signing: declared
                .signing
                .or_else(|| certificate::taken_by(info).then(|| certificate::PATH.to_string())),
        };
        (atelier, refused)
    }

    /// What each lets the app do, a line each, for the user to agree to.
    pub fn lines(&self) -> Vec<&'static str> {
        let mut lines = Vec::new();
        if self.pairing.is_some() {
            lines.push(
                "this iPhone's pairing, which lets it install apps and use the iPhone's developer services, as this program does",
            );
        }
        if self.signing.is_some() {
            lines.push(
                "your Apple ID, its password and this program's certificate, which let it sign in to Apple as you and sign apps",
            );
        }
        lines
    }
}

/// What `info` declares, when it declares something this program hands
/// over.
fn declared(info: &plist::Dictionary) -> Result<HandOffs, String> {
    let Some(value) = info.get(DECLARATION) else {
        return Ok(HandOffs::default());
    };
    let declaration = value
        .as_dictionary()
        .ok_or_else(|| format!("its {DECLARATION} isn't a dictionary"))?;
    if declaration.get("Version").and_then(version) != Some(VERSION) {
        return Err(format!(
            "its {DECLARATION} isn't version {VERSION}, the one this program hands over"
        ));
    }
    let path = |key: &str| match declaration.get(key) {
        None => Ok(None),
        Some(value) => {
            let raw = value
                .as_string()
                .ok_or_else(|| format!("its {key} isn't a path"))?;
            container_path(raw)
                .map(Some)
                .map_err(|why| format!("its {key}, {raw}, {why}"))
        }
    };
    let hand_offs = HandOffs {
        pairing: path(PAIRING_FILE)?,
        signing: path(SIGNING_IDENTITY)?,
    };
    if hand_offs.pairing.is_some() && hand_offs.pairing == hand_offs.signing {
        return Err(format!(
            "its {PAIRING_FILE} and {SIGNING_IDENTITY} are the same file"
        ));
    }
    Ok(hand_offs)
}

fn version(value: &plist::Value) -> Option<i64> {
    match value {
        plist::Value::Integer(version) => version.as_signed(),
        // As a build setting can put it.
        plist::Value::String(version) => version.trim().parse().ok(),
        _ => None,
    }
}

/// `raw` as a path in an app's container: relative, in its Documents,
/// Library or tmp folder, and never out of them.
pub fn container_path(raw: &str) -> Result<String, &'static str> {
    if raw.is_empty() || raw.len() > 1024 || raw.contains('\0') {
        return Err("isn't a path");
    }
    let parts: Vec<&str> = raw.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("isn't a plain path from the app's container");
    }
    if parts.len() < 2 || !TOP_FOLDERS.contains(&parts[0]) {
        return Err("isn't in the app's Documents, Library or tmp folder");
    }
    Ok(raw.to_string())
}

/// The folders to make for the file at `path`, from the container's root
/// down.
pub fn folders(path: &str) -> Vec<&str> {
    path.match_indices('/')
        .map(|(end, _)| &path[..end])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(entries: &[(&str, plist::Value)]) -> plist::Dictionary {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect()
    }

    fn declaration(entries: &[(&str, plist::Value)]) -> plist::Value {
        plist::Value::Dictionary(info(entries))
    }

    #[test]
    fn an_app_takes_what_it_declares() {
        let both = info(&[(
            DECLARATION,
            declaration(&[
                ("Version", 1.into()),
                (PAIRING_FILE, "Documents/pairingFile.plist".into()),
                (
                    SIGNING_IDENTITY,
                    "Library/Caches/Sideload/identity.plist".into(),
                ),
            ]),
        )]);
        assert_eq!(
            HandOffs::of("com.example.app", &both),
            (
                HandOffs {
                    pairing: Some("Documents/pairingFile.plist".into()),
                    signing: Some("Library/Caches/Sideload/identity.plist".into()),
                },
                None
            )
        );
        assert_eq!(HandOffs::of("com.example.app", &both).0.lines().len(), 2);

        let pairing_only = info(&[(
            DECLARATION,
            declaration(&[
                ("Version", "1".into()),
                (
                    PAIRING_FILE,
                    "Documents/pairing file/pairingFile.plist".into(),
                ),
            ]),
        )]);
        let (taken, refused) = HandOffs::of("com.example.app", &pairing_only);
        assert_eq!(
            taken.pairing.as_deref(),
            Some("Documents/pairing file/pairingFile.plist")
        );
        assert_eq!((taken.signing, refused), (None, None));
    }

    #[test]
    fn an_app_that_declares_nothing_takes_nothing() {
        let (taken, refused) = HandOffs::of("com.example.app", &info(&[]));
        assert!(taken.is_empty() && taken.lines().is_empty());
        assert_eq!(refused, None);
        // Atelier's own declaration is Atelier's alone.
        let atelier_declaration = info(&[(
            certificate::DECLARATION,
            plist::Value::Integer(certificate::VERSION.into()),
        )]);
        assert!(
            HandOffs::of("com.example.app", &atelier_declaration)
                .0
                .is_empty()
        );
    }

    #[test]
    fn atelier_takes_what_its_builds_read() {
        let (old, refused) = HandOffs::of(ATELIER, &info(&[]));
        assert_eq!(
            (old.pairing.as_deref(), old.signing, refused),
            (Some(ATELIER_PAIRING), None, None)
        );
        let takes_it = info(&[(
            certificate::DECLARATION,
            plist::Value::Integer(certificate::VERSION.into()),
        )]);
        assert_eq!(
            HandOffs::of(ATELIER, &takes_it).0.signing.as_deref(),
            Some(certificate::PATH)
        );
        let declares = info(&[(
            DECLARATION,
            declaration(&[
                ("Version", 1.into()),
                (PAIRING_FILE, "Documents/pairingFile.plist".into()),
            ]),
        )]);
        assert_eq!(
            HandOffs::of(ATELIER, &declares).0.pairing.as_deref(),
            Some("Documents/pairingFile.plist")
        );
    }

    #[test]
    fn refuses_a_declaration_it_cant_follow() {
        let refused = |entries: &[(&str, plist::Value)]| {
            let (taken, refused) = HandOffs::of(
                "com.example.app",
                &info(&[(DECLARATION, declaration(entries))]),
            );
            assert!(taken.is_empty());
            refused.unwrap()
        };
        let pairing = |path: &str| [("Version", 1.into()), (PAIRING_FILE, path.into())];
        assert!(refused(&[(PAIRING_FILE, "Documents/a".into())]).contains("version"));
        assert!(refused(&[("Version", 2.into())]).contains("version 1"));
        for path in [
            "/Documents/a",
            "Documents/../../a",
            "Documents//a",
            "Documents/./a",
            "Documents",
            "SystemData/a",
            "",
        ] {
            assert!(refused(&pairing(path)).contains("PairingFile"), "{path}");
        }
        let same = refused(&[
            ("Version", 1.into()),
            (PAIRING_FILE, "Documents/a".into()),
            (SIGNING_IDENTITY, "Documents/a".into()),
        ]);
        assert!(same.contains("same file"));
        let not_a_dictionary = HandOffs::of("com.example.app", &info(&[(DECLARATION, 1.into())]));
        assert!(not_a_dictionary.1.unwrap().contains("isn't a dictionary"));
        // Atelier still takes what its builds read.
        let (atelier, refused) = HandOffs::of(
            ATELIER,
            &info(&[(DECLARATION, declaration(&pairing("../a")))]),
        );
        assert_eq!(atelier.pairing.as_deref(), Some(ATELIER_PAIRING));
        assert!(refused.is_some());
    }

    #[test]
    fn makes_the_folders_a_file_goes_in() {
        assert_eq!(
            folders(ATELIER_PAIRING),
            [
                "Library",
                "Library/Application Support",
                "Library/Application Support/Pairing"
            ]
        );
        assert_eq!(folders("Documents/pairingFile.plist"), ["Documents"]);
    }
}
