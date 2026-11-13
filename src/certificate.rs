//! What this program hands Atelier with the pairing, so that Atelier opens
//! signed in and signs with the certificate this program signed it with
//! The certificate and its key, because a free Apple ID only gets
//! 2, and the program and Atelier shouldn't take both. The sign-in: the
//! Apple ID and password as typed here, the iPhone registered with them,
//! and the anisette server and state the sign-in went through, so that
//! Atelier signs in as the same machine and Apple doesn't ask for a code
//! again.
//!
//! None of it leaves the iPhone. It goes from this program to Atelier's
//! container through house_arrest, into its Caches folder, which no backup
//! takes, and Atelier moves it on its first launch, the password into its
//! Keychain. It only goes to an Atelier whose Info.plist says it takes it
//! ([`DECLARATION`]): one that doesn't would keep the password and the key
//! in its Caches. Atelier's side is in atelierstore/Atelier.

use std::io;
use std::path::Path;

use isideload::anisette::remote_v3::ANISETTE_STATE_KEY;
use isideload::dev::certificates::{CertificatesApi, DevelopmentCertificate};
use isideload::dev::developer_session::DeveloperSession;
use isideload::dev::teams::DeveloperTeam;
use isideload::util::storage::SideloadingStorage;
use rootcause::Report;
use rsa::RsaPrivateKey;
use rsa::pkcs1::EncodeRsaPublicKey;
use rsa::pkcs8::DecodePrivateKey;
use x509_cert::der::Decode;

use crate::state::AppleIdHash;

/// Where the hand-off goes in Atelier's container.
pub const PATH: &str = "Library/Caches/Installer/certificate.plist";
/// The key in Atelier's Info.plist that says it takes the hand-off, with
/// the version of the format it takes: [`VERSION`], this one.
pub const DECLARATION: &str = "AtelierInstallerHandOff";
pub const VERSION: i64 = 1;

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

/// What Atelier signs in with. Not `Debug`, so that the password can't
/// end up in the log.
pub struct SignIn {
    /// As typed here, lowercased.
    pub apple_id: String,
    pub password: String,
    /// The iPhone this program registered with the Apple ID's team.
    pub udid: String,
    /// The anisette server the sign-in went through, with isideload's
    /// provider at serial "0", as Atelier's.
    pub anisette_server: String,
    /// isideload's state for that server: its `anisette_state`, a plist.
    pub anisette_state: Vec<u8>,
}

/// isideload's anisette state in `folder`, its storage for one server.
pub fn anisette_state(folder: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(folder.join(ANISETTE_STATE_KEY))
}

impl Certificate {
    /// The hand-off file: an XML plist with the certificate's `teamID`,
    /// `machineName`, `machineID` and `serialNumber` (strings) and
    /// `certificate` and `privateKey` (data), and the sign-in's `appleID`,
    /// `password`, `udid` and `anisetteServer` (strings) and
    /// `anisetteState` (data).
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
        dictionary.insert("password".into(), sign_in.password.clone().into());
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

/// The name the signing key for `account` is kept under: isideload's,
/// for the Apple ID it signs for.
fn key_name(account: &AppleIdHash) -> String {
    format!("{}/key", account.as_str())
}

/// isideload's storage, for a signing that knows the Apple ID by its hash
/// alone, as when a run reuses the kept sign-in without asking for it.
/// isideload keeps the signing key under a hash of the Apple ID it's
/// given. Given the hash in its place, it would hash that again, so what
/// it keeps under the hash of `account`'s hash goes under `account`'s,
/// where it went when it was given the Apple ID.
pub struct ByAppleIdHash<S> {
    inner: S,
    given: String,
    account: String,
}

impl<S: SideloadingStorage> ByAppleIdHash<S> {
    pub fn new(inner: S, account: &AppleIdHash) -> Self {
        ByAppleIdHash {
            inner,
            given: AppleIdHash::of(account.as_str()).as_str().to_string(),
            account: account.as_str().to_string(),
        }
    }

    fn key(&self, key: &str) -> String {
        match key.strip_prefix(self.given.as_str()) {
            Some(rest) if rest.starts_with('/') => format!("{}{rest}", self.account),
            _ => key.to_string(),
        }
    }
}

impl<S: SideloadingStorage> SideloadingStorage for ByAppleIdHash<S> {
    fn store(&self, key: &str, value: &str) -> Result<(), Report> {
        self.inner.store(&self.key(key), value)
    }

    fn retrieve(&self, key: &str) -> Result<Option<String>, Report> {
        self.inner.retrieve(&self.key(key))
    }

    fn store_data(&self, key: &str, value: &[u8]) -> Result<(), Report> {
        self.inner.store_data(&self.key(key), value)
    }

    fn retrieve_data(&self, key: &str) -> Result<Option<Vec<u8>>, Report> {
        self.inner.retrieve_data(&self.key(key))
    }

    fn delete(&self, key: &str) -> Result<(), Report> {
        self.inner.delete(&self.key(key))
    }
}

/// The certificate isideload signed with for `account` in `team`: the one
/// Apple lists as made on `machine_name` with the key `storage` keeps, as
/// isideload finds it. None when there's no key, or Apple doesn't list the
/// certificate, as once it's revoked or expired.
pub async fn kept(
    storage: &dyn SideloadingStorage,
    developer: &mut DeveloperSession,
    team: &DeveloperTeam,
    account: &AppleIdHash,
    machine_name: &str,
) -> Result<Option<Certificate>, Report> {
    let Some(private_key) = storage.retrieve_data(&key_name(account))? else {
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

    /// sha256("someone@example.com"), as isideload names the key's folder.
    const HASH: &str = "72497f475e4f76d0b28f57c73a084ece576d170874eba3ee2609d9afe4b71aab";

    #[test]
    fn the_key_is_found_under_isideloads_name() {
        let account = AppleIdHash::of("someone@example.com");
        assert_eq!(key_name(&account), format!("{HASH}/key"));
    }

    /// isideload, given the hash for the Apple ID, keeps and finds the key
    /// where it did when given the Apple ID.
    #[test]
    fn a_signing_by_the_hash_keeps_the_key_where_the_apple_ids_went() {
        use isideload::util::storage::InMemoryStorage;

        let account = AppleIdHash::of("someone@example.com");
        let storage = ByAppleIdHash::new(InMemoryStorage::new(), &account);
        // isideload's own name for its key, given the hash.
        let rehashed = format!("{}/key", AppleIdHash::of(HASH).as_str());
        storage.store_data(&rehashed, b"key").unwrap();
        assert_eq!(
            storage.inner.retrieve_data(&format!("{HASH}/key")).unwrap(),
            Some(b"key".to_vec())
        );
        assert_eq!(
            storage.retrieve_data(&rehashed).unwrap(),
            Some(b"key".to_vec())
        );
        // Anything else, as it is.
        storage.store("anisette/state", "s").unwrap();
        assert_eq!(
            storage.inner.retrieve("anisette/state").unwrap(),
            Some("s".into())
        );
        let near = format!("{}x/key", AppleIdHash::of(HASH).as_str());
        storage.store(&near, "n").unwrap();
        assert_eq!(storage.inner.retrieve(&near).unwrap(), Some("n".into()));
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

    #[test]
    fn the_hand_off_has_what_atelier_reads() {
        let certificate = Certificate {
            team_id: "ABCDE12345".into(),
            machine_name: "Atelier Installer".into(),
            machine_id: "machine".into(),
            serial_number: "1A2B3C".into(),
            der: TEST_CERTIFICATE.to_vec(),
            private_key: vec![1, 2, 3],
        };
        let sign_in = SignIn {
            apple_id: "someone@example.com".into(),
            password: "not a real password".into(),
            udid: "00000000-0000000000000000".into(),
            anisette_server: "https://anisette.example.com".into(),
            anisette_state: vec![4, 5, 6],
        };
        let bytes = certificate.hand_off(&sign_in);
        assert!(bytes.starts_with(b"<?xml"));
        let value = plist::Value::from_reader_xml(bytes.as_slice()).unwrap();
        let dictionary = value.as_dictionary().unwrap();
        let mut keys: Vec<&str> = dictionary.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
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
