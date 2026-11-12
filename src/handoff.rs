//! Step 7, for any app: what an app takes from this program once it's
//! installed, and where in its container. The app says so in its
//! Info.plist ([`DECLARATION`]), and this program writes what it declares,
//! at the paths it declares, and nothing else. Nothing that comes with an
//! IPA runs here: running it in the program that holds the pairing and the
//! sign-in would be the dangerous kind of post-processing. An app that
//! declares nothing gets nothing, and any app but Atelier gets something
//! only once the user said yes to it.
//!
//! Atelier's builds from before the declaration read the pairing at
//! [`ATELIER_PAIRING`], and the certificate and the sign-in at
//! `certificate::PATH`, in version 1, when they say
//! `AtelierInstallerHandOff` (`certificate.rs`), so Atelier takes those
//! unless it declares others.

use crate::certificate::{self, Format};
use crate::ipa::ATELIER;

/// The key in an app's Info.plist that says what it takes:
///
/// ```xml
/// <key>SideloadHandOff</key>
/// <dict>
///     <key>Version</key>
///     <integer>2</integer>
///     <key>PairingFile</key>
///     <string>Documents/pairingFile.plist</string>
///     <key>SigningIdentity</key>
///     <string>Library/Caches/Installer/certificate.plist</string>
/// </dict>
/// ```
///
/// Either path can be left out. The pairing is an RPPairing file as
/// idevice writes it, in either version. The signing identity is
/// certificate.rs's format of the same version: with the password in
/// version 1, and with Apple's token in its place in version 2, so that
/// the password only goes to Apple. This program hands over either, as an
/// app declares, so that an app's builds from before version 2 still get
/// what they read.
pub const DECLARATION: &str = "SideloadHandOff";
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
    /// the Apple ID and its password or Apple's token, the iPhone, and the
    /// anisette server and state the sign-in went through (certificate.rs).
    pub signing: Option<String>,
    /// The signing identity's format, the version of it the app reads.
    pub format: Format,
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
        let (signing, format) = match declared.signing {
            Some(path) => (Some(path), declared.format),
            None if certificate::taken_by(info) => {
                (Some(certificate::PATH.to_string()), Format::Password)
            }
            None => (None, Format::default()),
        };
        let atelier = HandOffs {
            pairing: declared
                .pairing
                .or_else(|| Some(ATELIER_PAIRING.to_string())),
            signing,
            format,
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
            lines.push(match self.format {
                Format::Password => {
                    "your Apple ID, its password and this program's certificate, which let it sign in to Apple as you and sign apps"
                }
                Format::Token => {
                    "your Apple ID, Apple's token for this sign-in (not the password) and this program's certificate, which let it use Apple's developer services as you and sign apps, until Apple ends the token"
                }
            });
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
    let format = declaration
        .get("Version")
        .and_then(version)
        .and_then(Format::of_version)
        .ok_or_else(|| {
            format!("its {DECLARATION} isn't version 1 or 2, the ones this program hands over")
        })?;
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
    let signing = path(SIGNING_IDENTITY)?;
    let hand_offs = HandOffs {
        pairing: path(PAIRING_FILE)?,
        // Of no matter without a signing identity.
        format: if signing.is_some() {
            format
        } else {
            Format::default()
        },
        signing,
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
                    format: Format::Password,
                },
                None
            )
        );
        let lines = HandOffs::of("com.example.app", &both).0.lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("its password"));

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
    fn an_app_that_declares_version_2_takes_the_token() {
        let both = info(&[(
            DECLARATION,
            declaration(&[
                ("Version", 2.into()),
                (PAIRING_FILE, "Documents/pairingFile.plist".into()),
                (
                    SIGNING_IDENTITY,
                    "Library/Caches/Sideload/identity.plist".into(),
                ),
            ]),
        )]);
        let (taken, refused) = HandOffs::of("com.example.app", &both);
        assert_eq!(refused, None);
        assert_eq!(
            taken,
            HandOffs {
                pairing: Some("Documents/pairingFile.plist".into()),
                signing: Some("Library/Caches/Sideload/identity.plist".into()),
                format: Format::Token,
            }
        );
        let lines = taken.lines();
        assert!(lines[1].contains("token") && !lines[1].contains("its password"));

        // The pairing's the same in either version.
        let pairing_only = info(&[(
            DECLARATION,
            declaration(&[
                ("Version", "2".into()),
                (PAIRING_FILE, "Documents/pairingFile.plist".into()),
            ]),
        )]);
        let (taken, refused) = HandOffs::of("com.example.app", &pairing_only);
        assert_eq!(
            (taken.pairing.as_deref(), taken.signing, refused),
            (Some("Documents/pairingFile.plist"), None, None)
        );
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
        let taken = HandOffs::of(ATELIER, &takes_it).0;
        assert_eq!(
            (taken.signing.as_deref(), taken.format),
            (Some(certificate::PATH), Format::Password)
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
    fn atelier_takes_the_token_where_it_declares_version_2() {
        // As Atelier declares both, for this program's builds that only
        // hand over version 1, which refuse its SideloadHandOff.
        let v2 = |signing: Option<&str>| {
            let mut entries = vec![
                ("Version", plist::Value::from(2)),
                (PAIRING_FILE, ATELIER_PAIRING.into()),
            ];
            if let Some(path) = signing {
                entries.push((SIGNING_IDENTITY, path.into()));
            }
            info(&[
                (DECLARATION, declaration(&entries)),
                (
                    certificate::DECLARATION,
                    plist::Value::Integer(certificate::VERSION.into()),
                ),
            ])
        };
        let (taken, refused) = HandOffs::of(ATELIER, &v2(Some(certificate::PATH)));
        assert_eq!(refused, None);
        assert_eq!(
            taken,
            HandOffs {
                pairing: Some(ATELIER_PAIRING.into()),
                signing: Some(certificate::PATH.into()),
                format: Format::Token,
            }
        );
        // Without a SigningIdentity, its older declaration's version 1.
        let taken = HandOffs::of(ATELIER, &v2(None)).0;
        assert_eq!(
            (taken.signing.as_deref(), taken.format),
            (Some(certificate::PATH), Format::Password)
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
        assert!(refused(&[("Version", 3.into())]).contains("version 1 or 2"));
        assert!(refused(&[("Version", 0.into())]).contains("version 1 or 2"));
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
