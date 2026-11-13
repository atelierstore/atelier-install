//! The IPA: a file in rootshell's folder, a path, or an https link,
//! checked against a SHA-256 when one is given, and read for what it
//! installs, with the download on isideload's HTTP client, through
//! rootshell's TLS.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::blocking::run;
use crate::handoff::HandOffs;
use crate::page;
use crate::state::{self, State};
use crate::terminal::{self, Stop};

/// Atelier's bundle ID as it's built (`SelfUpdate.sourceBundleID`).
pub const ATELIER: &str = "com.atelier.app";

pub struct Ipa {
    pub path: PathBuf,
    pub sha256: String,
    pub bundle_id: String,
    pub name: String,
    pub version: String,
    /// What it takes from this program once it's installed (handoff.rs).
    pub hand_offs: HandOffs,
    /// Why what it declares it takes isn't handed over, when that's so.
    pub refused: Option<String>,
}

/// The IPA from `source`, or the one in rootshell's folder.
pub fn get(state: &State, source: Option<&str>, sha256: Option<&str>) -> Result<Ipa, Stop> {
    let (path, downloaded) = match source {
        Some(link) if link.starts_with("https://") => (kept_or_download(state, link)?, true),
        Some(link) if link.contains("://") => {
            return Err(Stop::new("The IPA's link has to start with https://"));
        }
        Some(path) => (PathBuf::from(path), false),
        None => (folders_ipa()?, false),
    };
    if !path.is_file() {
        return Err(Stop::new(format!("There's no file at {}", path.display())));
    }

    let digest = hex::encode(hash(&path)?);
    terminal::log(format!("ipa sha256 {digest}"));
    if let Some(expected) = sha256 {
        if !expected.trim().eq_ignore_ascii_case(&digest) {
            if downloaded {
                state.forget_download();
            }
            return Err(Stop::new("The IPA isn't the one this SHA-256 is for")
                .next(format!("It has {digest}."))
                .next("Download it again, or check the link."));
        }
        terminal::say("Its SHA-256 matches.");
    }

    let info =
        info(&path).map_err(|why| Stop::new(format!("{} isn't an IPA: {why}", path.display())))?;
    Ok(Ipa {
        path,
        sha256: digest,
        bundle_id: info.bundle_id,
        name: info.name,
        version: info.version,
        hand_offs: info.hand_offs,
        refused: info.refused,
    })
}

/// The only `.ipa` in rootshell's folder.
fn folders_ipa() -> Result<PathBuf, Stop> {
    let folder = state::files_folder();
    let mut found: Vec<PathBuf> = fs::read_dir(&folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("ipa"))
                        && path.is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    match found.len() {
        1 => {
            terminal::say(format!("Found {}.", name(&found[0])));
            Ok(found.remove(0))
        }
        0 => Err(Stop::new("There's no IPA to install")
            .next("In Files, put Atelier's IPA in rootshell's folder, then run this again.")
            .next("Or give its link: ./atelier-install.wasm --ipa https://…")),
        _ => {
            let mut stop = Stop::new("rootshell's folder holds more than one IPA");
            for path in &found {
                stop = stop.next(format!("./atelier-install.wasm --ipa \"{}\"", name(path)));
            }
            Err(stop)
        }
    }
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The IPA at `link`: the one the last run downloaded from it, when it's
/// there, or a new download.
fn kept_or_download(state: &State, link: &str) -> Result<PathBuf, Stop> {
    let kept = fs::read_to_string(state.download_link()).is_ok_and(|kept| kept == link);
    if kept && state.download().is_file() {
        terminal::say("Downloaded in the last run.");
        return Ok(state.download());
    }
    state.forget_download();
    let path = download(state, link)?;
    // Only once it's all there.
    let _ = fs::write(state.download_link(), link);
    Ok(path)
}

fn download(state: &State, link: &str) -> Result<PathBuf, Stop> {
    let failed = |error: isideload::http::Error| {
        terminal::log(format!("download: {error}"));
        Stop::new("Couldn't download the IPA")
            .next(error.without_url().to_string())
            .next("Check the link and the connection, then run this again.")
    };
    let path = state.download();
    let client = isideload::http::Client::new();
    let mut response = run(client.get(link).send())
        .expect("the HTTP client waits for nothing")
        .and_then(|response| response.error_for_status())
        .map_err(failed)?;
    let total = response.content_length();
    let mut file = File::create(&path)?;
    let mut received: u64 = 0;
    let mut shown = u64::MAX;
    while let Some(chunk) = run(response.chunk())
        .expect("the HTTP client waits for nothing")
        .map_err(failed)?
    {
        crate::awake::check_in();
        file.write_all(&chunk)?;
        received += chunk.len() as u64;
        let megabytes = received / (1 << 20);
        if megabytes != shown {
            shown = megabytes;
            match total {
                Some(total) if total > 0 => {
                    let of = format!("{megabytes} of {} MB", total.div_ceil(1 << 20));
                    terminal::bar(received as f64 / total as f64, &of);
                    page::bar(received as f64 / total as f64, of);
                }
                _ => terminal::progress(format!("Downloading: {megabytes} MB")),
            }
        }
        page::tick();
    }
    terminal::done(format!("Downloaded {} MB.", received.div_ceil(1 << 20)));
    Ok(path)
}

fn hash(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    // Big reads: each is a round trip to rootshell's app.
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// What the app in an IPA says of itself, in its Info.plist.
#[derive(Debug, PartialEq)]
struct Info {
    bundle_id: String,
    name: String,
    version: String,
    hand_offs: HandOffs,
    refused: Option<String>,
}

/// What the app in the IPA at `path` says of itself.
fn info(path: &Path) -> Result<Info, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|_| "it isn't a zip archive".to_string())?;
    let name = archive
        .file_names()
        .find(|name| is_app_info(name))
        .map(str::to_owned)
        .ok_or("it has no app in a Payload folder")?;
    let mut bytes = Vec::new();
    archive
        .by_name(&name)
        .and_then(|mut entry| Ok(entry.read_to_end(&mut bytes)?))
        .map_err(|_| "its Info.plist can't be read".to_string())?;
    let info: plist::Dictionary =
        plist::from_bytes(&bytes).map_err(|_| "its Info.plist can't be read".to_string())?;
    let text = |key: &str| {
        info.get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    };
    let bundle_id = text("CFBundleIdentifier").ok_or("its app has no bundle ID")?;
    let name = text("CFBundleDisplayName")
        .or_else(|| text("CFBundleName"))
        .unwrap_or_else(|| bundle_id.clone());
    let version = text("CFBundleShortVersionString")
        .or_else(|| text("CFBundleVersion"))
        .unwrap_or_default();
    let (hand_offs, refused) = HandOffs::of(&bundle_id, &info);
    Ok(Info {
        hand_offs,
        refused,
        bundle_id,
        name,
        version,
    })
}

/// `Payload/<name>.app/Info.plist`, the app's own and no bundle's inside it.
fn is_app_info(name: &str) -> bool {
    let mut parts = name.split('/');
    parts.next() == Some("Payload")
        && parts.next().is_some_and(|app| app.ends_with(".app"))
        && parts.next() == Some("Info.plist")
        && parts.next().is_none()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    pub(crate) fn ipa(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    pub(crate) fn info_plist(bundle_id: &str) -> Vec<u8> {
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert("CFBundleIdentifier".into(), bundle_id.into());
        dictionary.insert("CFBundleName".into(), "Atelier".into());
        dictionary.insert("CFBundleShortVersionString".into(), "0.5.0".into());
        let mut bytes = Vec::new();
        plist::Value::Dictionary(dictionary)
            .to_writer_binary(&mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    fn reads_the_apps_own_info() {
        let folder = crate::testing::scratch("ipa-info");
        let extension = info_plist("com.atelier.app.LiveActivity");
        let app = info_plist(ATELIER);
        let path = folder.join("good.ipa");
        fs::write(
            &path,
            ipa(&[
                (
                    "Payload/Atelier.app/PlugIns/LiveActivity.appex/Info.plist",
                    &extension,
                ),
                ("Payload/Atelier.app/Info.plist", &app),
            ]),
        )
        .unwrap();
        assert_eq!(
            info(&path).unwrap(),
            Info {
                bundle_id: ATELIER.to_string(),
                name: "Atelier".to_string(),
                version: "0.5.0".to_string(),
                hand_offs: HandOffs {
                    pairing: Some(crate::handoff::ATELIER_PAIRING.to_string()),
                    signing: None,
                    format: Default::default(),
                },
                refused: None,
            }
        );
        let not_zip = folder.join("text.ipa");
        fs::write(&not_zip, b"hello").unwrap();
        assert_eq!(info(&not_zip).unwrap_err(), "it isn't a zip archive");
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn sees_what_the_app_takes() {
        let folder = crate::testing::scratch("ipa-hand-off");
        let declares = |bundle_id: &str| {
            let mut app = plist::Dictionary::new();
            app.insert("CFBundleIdentifier".into(), bundle_id.into());
            app.insert(
                crate::certificate::DECLARATION.into(),
                plist::Value::Integer(crate::certificate::VERSION.into()),
            );
            let mut bytes = Vec::new();
            plist::Value::Dictionary(app)
                .to_writer_binary(&mut bytes)
                .unwrap();
            let path = folder.join(format!("{bundle_id}.ipa"));
            fs::write(&path, ipa(&[("Payload/App.app/Info.plist", &bytes)])).unwrap();
            info(&path).unwrap().hand_offs.signing
        };
        assert_eq!(declares(ATELIER).as_deref(), Some(crate::certificate::PATH));
        // Atelier's declaration is Atelier's alone (handoff.rs).
        assert_eq!(declares("com.example.other"), None);
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn hashes_the_file() {
        let folder = crate::testing::scratch("ipa-hash");
        let path = folder.join("hash");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            hex::encode(hash(&path).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_dir_all(folder).unwrap();
    }
}
