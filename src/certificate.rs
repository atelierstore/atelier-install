//! What this program hands Atelier with the pairing, so that Atelier opens
//! signed in and signs with the certificate this program signed it with.
//! The certificate and its key, because a free Apple ID only gets
//! 2, and the program and Atelier shouldn't take both. The sign-in: the
//! Apple ID as typed here, what it signs in with, the iPhone registered
//! with it, and the anisette server and state the sign-in went through, so
//! that Atelier signs in as the same machine and Apple doesn't ask for a
//! code again.
//!
//! What it signs in with depends on the format the app takes ([`Format`]):
//! the password, in version 1, or Apple's token for its developer services
//! in the password's place, in version 2 (token.rs), so that the password
//! only goes to Apple.
//!
//! None of it leaves the iPhone. It goes from this program to Atelier's
//! container through house_arrest, into its Caches folder, which no backup
//! takes, and Atelier moves it on its first launch, the password or the
//! token into its Keychain. It only goes to an Atelier whose Info.plist
//! says it takes it ([`DECLARATION`], or handoff.rs's): one that doesn't
//! would keep the password or the token, and the key, in its Caches.
//! Atelier's side is in atelierstore/Atelier.

use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use isideload::anisette::remote_v3::ANISETTE_STATE_KEY;
use isideload::dev::certificates::{CertificatesApi, DevelopmentCertificate};
use isideload::dev::developer_session::DeveloperSession;
use isideload::dev::teams::DeveloperTeam;
use isideload::util::storage::SideloadingStorage;
use rootcause::Report;
use rsa::RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPublicKey;
use rsa::pkcs8::DecodePrivateKey;
use sha2::{Digest, Sha256};
use x509_cert::der::Decode;

use crate::token::Token;

/// Where the hand-off goes in Atelier's container.
pub const PATH: &str = "Library/Caches/Installer/certificate.plist";
/// The key in Atelier's Info.plist that says it takes the hand-off, from
/// its builds before `SideloadHandOff` (handoff.rs), with the version of
/// the format it takes: [`VERSION`], the first, with the password.
pub const DECLARATION: &str = "AtelierInstallerHandOff";
pub const VERSION: i64 = 1;

/// The hand-off's formats, by what the app signs in with.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Version 1: the password.
    #[default]
    Password,
    /// Version 2: Apple's token for its developer services, with the
    /// account's adsid, in the password's place.
    Token,
}

impl Format {
    pub fn version(self) -> i64 {
        match self {
            Format::Password => 1,
            Format::Token => 2,
        }
    }

    pub fn of_version(version: i64) -> Option<Format> {
        match version {
            1 => Some(Format::Password),
            2 => Some(Format::Token),
            _ => None,
        }
    }
}

/// Whether the app whose Info.plist is `info` takes this hand-off.
pub fn taken_by(info: &plist::Dictionary) -> bool {
    match info.get(DECLARATION) {
        Some(plist::Value::Integer(version)) => version.as_signed() == Some(VERSION),
        // As a build setting can put it.
        Some(plist::Value::String(version)) => version.trim().parse::<i64>().ok() == Some(VERSION),
        _ => false,
    }
}

/// A development certificate and its key.
#[derive(Debug, Clone, PartialEq)]
pub struct Certificate {
    pub team_id: String,
    /// What Apple lists it as made on: this program's name.
    pub machine_name: String,
    pub machine_id: String,
    pub serial_number: String,
    /// The certificate, DER, as Apple lists it.
    pub der: Vec<u8>,
    /// Its key, PKCS#8 DER, as isideload keeps it.
    pub private_key: Vec<u8>,
}

/// What Atelier signs in with. Not `Debug`, so that the password and the
/// token can't end up in the log.
pub struct SignIn {
    /// As typed here, lowercased.
    pub apple_id: String,
    pub secret: Secret,
    /// The iPhone this program registered with the Apple ID's team.
    pub udid: String,
    /// The anisette server the sign-in went through, with isideload's
    /// provider at serial "0", as Atelier's.
    pub anisette_server: String,
    /// isideload's state for that server: its `anisette_state`, a plist.
    pub anisette_state: Vec<u8>,
}

/// The password, or Apple's token: what the format the app takes holds.
pub enum Secret {
    Password(String),
    Token(Token),
}

/// isideload's anisette state in `folder`, its storage for one server.
pub fn anisette_state(folder: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(folder.join(ANISETTE_STATE_KEY))
}

impl Certificate {
    /// The hand-off file: an XML plist with the certificate's `teamID`,
    /// `machineName`, `machineID` and `serialNumber` (strings) and
    /// `certificate` and `privateKey` (data), and the sign-in's `appleID`,
    /// `udid` and `anisetteServer` (strings) and `anisetteState` (data).
    ///
    /// In version 1, `password` (a string) is what the app signs in with.
    /// Version 2 says its `version` (an integer, 2), and has Apple's token
    /// in the password's place: `adsid` and `token` (strings),
    /// `tokenDuration` and `tokenExpiry` (integers, as Apple gave them),
    /// `tokenReceived` (a date), and, when the sign-in sent Apple its own
    /// in place of the server's, `clientInfo` and `userAgent` (strings).
    pub fn hand_off(&self, sign_in: &SignIn) -> Vec<u8> {
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert("teamID".into(), self.team_id.clone().into());
        dictionary.insert("machineName".into(), self.machine_name.clone().into());
        dictionary.insert("machineID".into(), self.machine_id.clone().into());
        dictionary.insert("serialNumber".into(), self.serial_number.clone().into());
        dictionary.insert("certificate".into(), plist::Value::Data(self.der.clone()));
        dictionary.insert(
            "privateKey".into(),
            plist::Value::Data(self.private_key.clone()),
        );
        dictionary.insert("appleID".into(), sign_in.apple_id.clone().into());
        match &sign_in.secret {
            Secret::Password(password) => {
                dictionary.insert("password".into(), password.clone().into());
            }
            Secret::Token(token) => {
                let version = Format::Token.version();
                dictionary.insert("version".into(), plist::Value::Integer(version.into()));
                dictionary.insert("adsid".into(), token.adsid.clone().into());
                dictionary.insert("token".into(), token.token.clone().into());
                dictionary.insert(
                    "tokenDuration".into(),
                    plist::Value::Integer(token.duration.into()),
                );
                dictionary.insert(
                    "tokenExpiry".into(),
                    plist::Value::Integer(token.expiry.into()),
                );
                let received = UNIX_EPOCH + Duration::from_secs(token.saved_at);
                dictionary.insert("tokenReceived".into(), plist::Value::Date(received.into()));
                if let Some((client_info, user_agent)) = &token.client_info {
                    dictionary.insert("clientInfo".into(), client_info.clone().into());
                    dictionary.insert("userAgent".into(), user_agent.clone().into());
                }
            }
        }
        dictionary.insert("udid".into(), sign_in.udid.clone().into());
        dictionary.insert(
            "anisetteServer".into(),
            sign_in.anisette_server.clone().into(),
        );
        dictionary.insert(
            "anisetteState".into(),
            plist::Value::Data(sign_in.anisette_state.clone()),
        );
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dictionary)
            .to_writer_xml(&mut bytes)
            .expect("writing a plist in memory");
        bytes
    }
}

/// The name isideload keeps the key for `email` under: the same email it
/// signed in with.
fn key_name(email: &str) -> String {
    format!("{}/key", hex::encode(Sha256::digest(email.as_bytes())))
}

/// The certificate isideload signed with for `email` in `team`: the one
/// Apple lists as made on `machine_name` with the key `storage` keeps, as
/// isideload finds it. None when there's no key, or Apple doesn't list the
/// certificate, as once it's revoked or expired.
pub async fn kept(
    storage: &dyn SideloadingStorage,
    developer: &mut DeveloperSession,
    team: &DeveloperTeam,
    email: &str,
    machine_name: &str,
) -> Result<Option<Certificate>, Report> {
    let Some(private_key) = storage.retrieve_data(&key_name(email))? else {
        return Ok(None);
    };
    let public_key = RsaPrivateKey::from_pkcs8_der(&private_key)?
        .to_public_key()
        .to_pkcs1_der()?;
    let listed = developer.list_ios_certs(team).await?;
    Ok(listed
        .iter()
        .find(|listed| {
            listed.machine_name.as_deref() == Some(machine_name)
                && listed
                    .cert_content
                    .as_ref()
                    .is_some_and(|der| holds_key(der.as_ref(), public_key.as_bytes()))
        })
        .and_then(|listed| certificate(listed, &team.team_id, private_key)))
}

/// `listed` with its key, when Apple gave all of it.
fn certificate(
    listed: &DevelopmentCertificate,
    team_id: &str,
    private_key: Vec<u8>,
) -> Option<Certificate> {
    Some(Certificate {
        team_id: team_id.to_string(),
        machine_name: listed.machine_name.clone()?,
        machine_id: listed.machine_id.clone()?,
        serial_number: listed.serial_number.clone()?,
        der: listed.cert_content.as_ref()?.as_ref().to_vec(),
        private_key,
    })
}

/// Whether `certificate`, DER, is for the RSA public key `public_key`,
/// PKCS#1 DER: the comparison isideload makes.
fn holds_key(certificate: &[u8], public_key: &[u8]) -> bool {
    x509_cert::Certificate::from_der(certificate)
        .ok()
        .and_then(|certificate| {
            certificate
                .tbs_certificate
                .subject_public_key_info
                .subject_public_key
                .as_bytes()
                .map(|bytes| bytes == public_key)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway self-signed certificate and its public key, PKCS#1 DER,
    // made with openssl for these tests; the private key wasn't kept.
    const TEST_CERTIFICATE: &[u8] = include_bytes!("../tests/data/test-certificate.der");
    const TEST_PUBLIC_KEY: &[u8] = include_bytes!("../tests/data/test-public-key.der");

    #[test]
    fn the_key_is_found_under_isideloads_name() {
        // sha256("someone@example.com")
        assert_eq!(
            key_name("someone@example.com"),
            "72497f475e4f76d0b28f57c73a084ece576d170874eba3ee2609d9afe4b71aab/key"
        );
    }

    #[test]
    fn a_certificate_holds_its_own_key_only() {
        assert!(holds_key(TEST_CERTIFICATE, TEST_PUBLIC_KEY));
        let mut other = TEST_PUBLIC_KEY.to_vec();
        let last = other.len() - 10;
        other[last] ^= 1;
        assert!(!holds_key(TEST_CERTIFICATE, &other));
        assert!(!holds_key(b"not a certificate", TEST_PUBLIC_KEY));
    }

    fn test_certificate() -> Certificate {
        Certificate {
            team_id: "ABCDE12345".into(),
            machine_name: "Atelier Installer".into(),
            machine_id: "machine".into(),
            serial_number: "1A2B3C".into(),
            der: TEST_CERTIFICATE.to_vec(),
            private_key: vec![1, 2, 3],
        }
    }

    fn test_sign_in(secret: Secret) -> SignIn {
        SignIn {
            apple_id: "someone@example.com".into(),
            secret,
            udid: "00000000-0000000000000000".into(),
            anisette_server: "https://anisette.example.com".into(),
            anisette_state: vec![4, 5, 6],
        }
    }

    fn keys(dictionary: &plist::Dictionary) -> Vec<&str> {
        let mut keys: Vec<&str> = dictionary.keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn the_hand_off_has_what_atelier_reads() {
        let sign_in = test_sign_in(Secret::Password("not a real password".into()));
        let bytes = test_certificate().hand_off(&sign_in);
        assert!(bytes.starts_with(b"<?xml"));
        let value = plist::Value::from_reader_xml(bytes.as_slice()).unwrap();
        let dictionary = value.as_dictionary().unwrap();
        // Version 1 as Atelier's builds from 1060 read it, without a version.
        assert_eq!(
            keys(dictionary),
            [
                "anisetteServer",
                "anisetteState",
                "appleID",
                "certificate",
                "machineID",
                "machineName",
                "password",
                "privateKey",
                "serialNumber",
                "teamID",
                "udid"
            ]
        );
        assert_eq!(dictionary["teamID"].as_string(), Some("ABCDE12345"));
        assert_eq!(
            dictionary["machineName"].as_string(),
            Some("Atelier Installer")
        );
        assert_eq!(dictionary["certificate"].as_data(), Some(TEST_CERTIFICATE));
        assert_eq!(dictionary["privateKey"].as_data(), Some(&[1u8, 2, 3][..]));
        assert_eq!(
            dictionary["appleID"].as_string(),
            Some("someone@example.com")
        );
        assert_eq!(
            dictionary["password"].as_string(),
            Some("not a real password")
        );
        assert_eq!(
            dictionary["udid"].as_string(),
            Some("00000000-0000000000000000")
        );
        assert_eq!(
            dictionary["anisetteServer"].as_string(),
            Some("https://anisette.example.com")
        );
        assert_eq!(
            dictionary["anisetteState"].as_data(),
            Some(&[4u8, 5, 6][..])
        );
    }

    #[test]
    fn version_2_has_apples_token_in_the_passwords_place() {
        let mut token = Token {
            server: "https://anisette.example.com".into(),
            client_info: Some((
                "<MacBookPro13,2> <macOS;13.1;22C65>".into(),
                "akd/1.0".into(),
            )),
            adsid: "000123-45-6789".into(),
            token: "not a real token".into(),
            duration: 31_536_000,
            expiry: 1_826_036_000_000,
            saved_at: 1_794_500_000,
        };
        let sign_in = test_sign_in(Secret::Token(token.clone()));
        let bytes = test_certificate().hand_off(&sign_in);
        let value = plist::Value::from_reader_xml(bytes.as_slice()).unwrap();
        let dictionary = value.as_dictionary().unwrap();
        assert_eq!(
            keys(dictionary),
            [
                "adsid",
                "anisetteServer",
                "anisetteState",
                "appleID",
                "certificate",
                "clientInfo",
                "machineID",
                "machineName",
                "privateKey",
                "serialNumber",
                "teamID",
                "token",
                "tokenDuration",
                "tokenExpiry",
                "tokenReceived",
                "udid",
                "userAgent",
                "version"
            ]
        );
        assert!(!String::from_utf8_lossy(&bytes).contains("password"));
        assert_eq!(dictionary["version"].as_signed_integer(), Some(2));
        assert_eq!(dictionary["adsid"].as_string(), Some("000123-45-6789"));
        assert_eq!(dictionary["token"].as_string(), Some("not a real token"));
        assert_eq!(
            dictionary["tokenDuration"].as_unsigned_integer(),
            Some(31_536_000)
        );
        assert_eq!(
            dictionary["tokenExpiry"].as_unsigned_integer(),
            Some(1_826_036_000_000)
        );
        let received = std::time::SystemTime::from(dictionary["tokenReceived"].as_date().unwrap());
        assert_eq!(
            received.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_794_500_000
        );
        assert_eq!(
            dictionary["clientInfo"].as_string(),
            Some("<MacBookPro13,2> <macOS;13.1;22C65>")
        );
        assert_eq!(dictionary["userAgent"].as_string(), Some("akd/1.0"));
        assert_eq!(
            dictionary["appleID"].as_string(),
            Some("someone@example.com")
        );

        // The server's own client info: neither key.
        token.client_info = None;
        let bytes = test_certificate().hand_off(&test_sign_in(Secret::Token(token)));
        let value = plist::Value::from_reader_xml(bytes.as_slice()).unwrap();
        let dictionary = value.as_dictionary().unwrap();
        assert!(!dictionary.contains_key("clientInfo") && !dictionary.contains_key("userAgent"));
    }

    #[test]
    fn knows_its_formats_by_version() {
        assert_eq!(Format::of_version(1), Some(Format::Password));
        assert_eq!(Format::of_version(2), Some(Format::Token));
        assert_eq!(Format::of_version(3), None);
        assert_eq!(Format::of_version(0), None);
        for format in [Format::Password, Format::Token] {
            assert_eq!(Format::of_version(format.version()), Some(format));
        }
        assert_eq!(Format::default().version(), VERSION);
    }

    #[test]
    fn it_goes_only_where_the_info_plist_says_it_s_taken() {
        let info = |value: Option<plist::Value>| {
            let mut dictionary = plist::Dictionary::new();
            dictionary.insert("CFBundleIdentifier".into(), "com.atelier.app".into());
            if let Some(value) = value {
                dictionary.insert(DECLARATION.into(), value);
            }
            dictionary
        };
        assert!(taken_by(&info(Some(plist::Value::Integer(1i64.into())))));
        assert!(taken_by(&info(Some(plist::Value::String("1".into())))));
        assert!(!taken_by(&info(None)));
        assert!(!taken_by(&info(Some(plist::Value::Integer(2i64.into())))));
        assert!(!taken_by(&info(Some(plist::Value::Boolean(true)))));
    }
}
