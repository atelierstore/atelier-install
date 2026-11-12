//! A stand-in iPhone for atelier-install's tests.
//!
//! Its side of device-initiated pair-setup, as idevice models it (its SRP
//! client, then the HKDF, ChaCha20-Poly1305 and Ed25519 steps of M5 and M6):
//! the simulated phone of Atelier's own tests, made to dial a program
//! that's already waiting to pair. It shows a program drives
//! idevice's `PairableHost` through the whole exchange to a pairing file
//! that names the same host the phone stored. Whether iOS takes the host is
//! only known on an iPhone.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use idevice::remote_pairing::{RpPairingSocket, RpPairingSocketProvider};
use idevice_srp::{client::SrpClient, groups::G_3072};
use rand::RngCore;
use serde_json::json;
use sha2::Sha512;
use tokio::net::TcpStream;

// Pair-setup TLV types.
const METHOD: u8 = 0x00;
const IDENTIFIER: u8 = 0x01;
const SALT: u8 = 0x02;
const PUBLIC_KEY: u8 = 0x03;
const PROOF: u8 = 0x04;
const ENCRYPTED_DATA: u8 = 0x05;
const STATE: u8 = 0x06;
const SIGNATURE: u8 = 0x0a;
const INFO: u8 = 0x11;

pub const PHONE_NAME: &str = "Test iPhone";
pub const PHONE_MODEL: &str = "iPhone16,1";

/// The host as the phone stores it once paired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedHost {
    /// The host's pairing identifier.
    pub identifier: String,
    /// Its long-term public key.
    pub public_key: [u8; 32],
    /// The altIRK the phone gave the host, which the pairing file keeps.
    pub phone_alt_irk: [u8; 16],
}

/// Why the phone didn't pair.
pub type Problem = String;

/// The phone's side of the conversation.
pub struct Phone {
    sock: RpPairingSocket<TcpStream>,
    seq: usize,
}

impl Phone {
    pub async fn connect(address: &str, port: u16) -> Result<Self, Problem> {
        let stream = TcpStream::connect((address, port))
            .await
            .map_err(|e| format!("connect to {address}:{port}: {e}"))?;
        Ok(Self {
            sock: RpPairingSocket::new(stream),
            seq: 0,
        })
    }

    async fn send(&mut self, value: serde_json::Value) -> Result<(), Problem> {
        self.sock
            .send_plain(value, self.seq)
            .await
            .map_err(|e| format!("send: {e}"))?;
        self.seq += 1;
        Ok(())
    }

    async fn recv(&mut self) -> Result<plist::Value, Problem> {
        tokio::time::timeout(Duration::from_secs(20), self.sock.recv_plain())
            .await
            .map_err(|_| "the host didn't answer".to_string())?
            .map_err(|e| format!("recv: {e}"))
    }

    /// A first-time handshake (no pair-verify), so the host must offer pair-setup.
    pub async fn handshake(&mut self) -> Result<(), Problem> {
        self.send(json!({
            "request": { "_0": { "handshake": { "_0": {
                "wireProtocolVersion": 26,
                "hostOptions": {},
            }}}}
        }))
        .await?;
        let reply = self.recv().await?;
        let allows = at(
            &reply,
            &[
                "response",
                "_1",
                "handshake",
                "_0",
                "deviceOptions",
                "allowsPairSetup",
            ],
        )
        .and_then(|v| v.as_boolean());
        if allows != Some(true) {
            return Err(format!("the host doesn't offer pair-setup: {reply:?}"));
        }
        Ok(())
    }

    async fn send_tlv(&mut self, data: &[u8], start_new_session: bool) -> Result<(), Problem> {
        self.send(json!({
            "event": { "_0": { "pairingData": { "_0": {
                "data": B64.encode(data),
                "startNewSession": start_new_session,
                "kind": "setupManualPairing",
            }}}}
        }))
        .await
    }

    async fn recv_tlv(&mut self) -> Result<Vec<(u8, Vec<u8>)>, Problem> {
        let message = self.recv().await?;
        let data = at(&message, &["event", "_0", "pairingData", "_0", "data"])
            .and_then(|v| v.as_string())
            .ok_or_else(|| format!("not a pairingData event: {message:?}"))?;
        Ok(parse_tlv(
            &B64.decode(data).map_err(|e| format!("base64 TLV: {e}"))?,
        ))
    }

    /// Sends M1, which is when the host makes its code, and returns M2's
    /// salt and SRP public key.
    pub async fn start_pair_setup(&mut self) -> Result<(Vec<u8>, Vec<u8>), Problem> {
        self.send_tlv(&tlv(&[(METHOD, &[0]), (STATE, &[1])]), true)
            .await?;
        let m2 = self.recv_tlv().await?;
        let (salt, b_pub) = (field(&m2, SALT), field(&m2, PUBLIC_KEY));
        if field(&m2, STATE) != [2] || salt.len() != 16 || b_pub.len() != 384 {
            return Err("M2 isn't a salt and an SRP public key".into());
        }
        Ok((salt, b_pub))
    }

    /// Pair-setup M3 to M6 with the code the user typed, after
    /// [`Self::start_pair_setup`]'s `(salt, b_pub)`.
    pub async fn finish_pair_setup(
        &mut self,
        code: &str,
        salt: &[u8],
        b_pub: &[u8],
    ) -> Result<PairedHost, Problem> {
        // M3/M4: SRP with the code.
        let srp = SrpClient::<Sha512>::new(&G_3072);
        let a = random::<32>();
        let verifier = srp
            .process_reply(&a, b"Pair-Setup", code.as_bytes(), salt, b_pub, false)
            .map_err(|e| format!("SRP: {e}"))?;
        let a_pub = srp.compute_public_ephemeral(&a);
        self.send_tlv(
            &tlv(&[
                (STATE, &[3]),
                (PUBLIC_KEY, &a_pub),
                (PROOF, verifier.proof()),
            ]),
            false,
        )
        .await?;
        let m4 = self.recv_tlv().await?;
        if field(&m4, STATE) != [4] || verifier.verify_server(&field(&m4, PROOF)).is_err() {
            return Err("the host refused the code".into());
        }
        let session_key = verifier.key().to_vec();
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&hkdf32(
            &session_key,
            b"Pair-Setup-Encrypt-Salt",
            b"Pair-Setup-Encrypt-Info",
        )));

        // M5: the phone's identity, encrypted with the SRP session key.
        let phone_key = SigningKey::from_bytes(&random::<32>());
        let phone_id = b"phone-pairing-identifier";
        let phone_ltpk = phone_key.verifying_key().to_bytes();
        let phone_x = hkdf32(
            &session_key,
            b"Pair-Setup-Controller-Sign-Salt",
            b"Pair-Setup-Controller-Sign-Info",
        );
        let phone_signature = phone_key.sign(&[&phone_x[..], phone_id, &phone_ltpk].concat());
        let phone_alt_irk = random::<16>();
        let info = opack_dict(&[
            ("accountID", Opack::Str("phone-account")),
            ("altIRK", Opack::Data(&phone_alt_irk)),
            ("model", Opack::Str(PHONE_MODEL)),
            ("name", Opack::Str(PHONE_NAME)),
            (
                "remotepairing_udid",
                Opack::Str("00008130-000A1B2C3D4E5F60"),
            ),
        ]);
        let m5_plain = tlv(&[
            (IDENTIFIER, phone_id),
            (PUBLIC_KEY, &phone_ltpk),
            (SIGNATURE, &phone_signature.to_bytes()),
            (INFO, &info),
        ]);
        let m5_sealed = cipher
            .encrypt(
                Nonce::from_slice(b"\0\0\0\0PS-Msg05"),
                Payload {
                    msg: &m5_plain,
                    aad: b"",
                },
            )
            .map_err(|_| "encrypt M5".to_string())?;
        self.send_tlv(&tlv(&[(STATE, &[5]), (ENCRYPTED_DATA, &m5_sealed)]), false)
            .await?;

        // M6: the host's identity, which the phone checks before storing it.
        let m6 = self.recv_tlv().await?;
        if field(&m6, STATE) != [6] {
            return Err("no M6".into());
        }
        let m6_plain = cipher
            .decrypt(
                Nonce::from_slice(b"\0\0\0\0PS-Msg06"),
                Payload {
                    msg: &field(&m6, ENCRYPTED_DATA),
                    aad: b"",
                },
            )
            .map_err(|_| "decrypt M6".to_string())?;
        let m6 = parse_tlv(&m6_plain);
        let host_id = field(&m6, IDENTIFIER);
        let host_ltpk: [u8; 32] = field(&m6, PUBLIC_KEY)
            .try_into()
            .map_err(|_| "the host's key isn't 32 bytes".to_string())?;
        let host_signature = Signature::from_slice(&field(&m6, SIGNATURE))
            .map_err(|_| "the host's signature isn't 64 bytes".to_string())?;
        let host_x = hkdf32(
            &session_key,
            b"Pair-Setup-Accessory-Sign-Salt",
            b"Pair-Setup-Accessory-Sign-Info",
        );
        VerifyingKey::from_bytes(&host_ltpk)
            .map_err(|_| "the host's key isn't a key".to_string())?
            .verify(
                &[&host_x[..], &host_id, &host_ltpk].concat(),
                &host_signature,
            )
            .map_err(|_| "the host's signature over its identity is wrong".to_string())?;
        Ok(PairedHost {
            identifier: String::from_utf8(host_id)
                .map_err(|_| "the host's identifier isn't text".to_string())?,
            public_key: host_ltpk,
            phone_alt_irk,
        })
    }
}

/// TLV8, splitting values longer than 255 bytes into consecutive items.
fn tlv(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(kind, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[kind, 0]);
        }
        for chunk in value.chunks(255) {
            out.push(kind);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

fn parse_tlv(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut items = Vec::new();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let (kind, len) = (bytes[i], bytes[i + 1] as usize);
        let end = (i + 2 + len).min(bytes.len());
        items.push((kind, bytes[i + 2..end].to_vec()));
        i = end;
    }
    items
}

/// A TLV field, with split items joined back together.
fn field(items: &[(u8, Vec<u8>)], kind: u8) -> Vec<u8> {
    items
        .iter()
        .filter(|(k, _)| *k == kind)
        .flat_map(|(_, v)| v.clone())
        .collect()
}

enum Opack<'a> {
    Str(&'a str),
    Data(&'a [u8]),
}

/// OPACK for a small dictionary of strings and data: what the phone's identity
/// info in M5 needs, encoded the way idevice decodes it.
fn opack_dict(entries: &[(&str, Opack)]) -> Vec<u8> {
    assert!(entries.len() < 15);
    let mut out = vec![0xE0 + entries.len() as u8];
    for (key, value) in entries {
        opack_item(&mut out, 0x40, 0x61, key.as_bytes());
        match value {
            Opack::Str(s) => opack_item(&mut out, 0x40, 0x61, s.as_bytes()),
            Opack::Data(d) => opack_item(&mut out, 0x70, 0x91, d),
        }
    }
    out
}

/// Up to 32 bytes, the length is part of the tag; up to 255, it's one byte after it.
fn opack_item(out: &mut Vec<u8>, inline_tag: u8, u8_len_tag: u8, bytes: &[u8]) {
    if bytes.len() <= 0x20 {
        out.push(inline_tag + bytes.len() as u8);
    } else {
        out.push(u8_len_tag);
        out.push(u8::try_from(bytes.len()).expect("item under 256 bytes"));
    }
    out.extend_from_slice(bytes);
}

fn hkdf32(secret: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha512>::new(Some(salt), secret)
        .expand(info, &mut out)
        .expect("HKDF expand");
    out
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn at<'a>(value: &'a plist::Value, keys: &[&str]) -> Option<&'a plist::Value> {
    keys.iter()
        .try_fold(value, |v, k| v.as_dictionary()?.get(k))
}
