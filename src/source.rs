//! Atelier's newest release, as Atelier's source lists it: what the page
//! (`--page`) installs, with the SHA-256 the source gives, so that the
//! Shortcut's line never changes from one release to the next.

use serde_json::Value;

use crate::blocking::run;
use crate::ipa;
use crate::terminal::{self, Stop};

/// The source Atelier's own updates read (`SelfUpdate`), and its website.
pub const SOURCE: &str =
    "https://raw.githubusercontent.com/atelierstore/Atelier/source/source.json";
/// Where Atelier's releases are: a link to anywhere else isn't taken.
const RELEASES: &str = "https://github.com/atelierstore/Atelier/releases/download/";
/// More than any source holds.
const LONGEST: usize = 1 << 20;

#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    pub link: String,
    pub sha256: String,
    /// As people know it, such as 0.7.0-alpha.
    pub version: String,
}

/// Atelier's newest release.
pub fn newest() -> Result<Release, Stop> {
    let failed = |why: String| {
        terminal::log(format!("Atelier's source: {why}"));
        Stop::new("Couldn't read Atelier's newest release from its source")
            .next("Check the connection, then run the Shortcut again.")
    };
    let client = isideload::http::Client::new();
    let mut response = run(client.get(SOURCE).send())
        .expect("the HTTP client waits for nothing")
        .and_then(|response| response.error_for_status())
        .map_err(|error| failed(error.to_string()))?;
    let mut body = Vec::new();
    while let Some(chunk) = run(response.chunk())
        .expect("the HTTP client waits for nothing")
        .map_err(|error| failed(error.to_string()))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > LONGEST {
            return Err(failed("longer than a source is".into()));
        }
    }
    let release = read(&body).map_err(failed)?;
    terminal::say(format!(
        "Atelier's newest release is {}, from its source.",
        release.version
    ));
    Ok(release)
}

/// Atelier's newest release in `source`, as AltStore's sources list
/// releases: the app's versions, the newest first.
fn read(source: &[u8]) -> Result<Release, String> {
    let source: Value = serde_json::from_slice(source).map_err(|error| error.to_string())?;
    let atelier = source["apps"]
        .as_array()
        .and_then(|apps| {
            apps.iter()
                .find(|app| app["bundleIdentifier"] == ipa::ATELIER)
        })
        .ok_or("it doesn't list Atelier")?;
    let newest = &atelier["versions"][0];
    let text = |name: &str| newest[name].as_str().filter(|text| !text.is_empty());
    let link = text("downloadURL").ok_or("its newest version has no link")?;
    if !link.starts_with(RELEASES) || link.contains("..") {
        return Err(format!("its link isn't one of Atelier's releases: {link}"));
    }
    let sha256 = text("sha256").ok_or("its newest version has no SHA-256")?;
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("its SHA-256 isn't 64 hexadecimal characters".into());
    }
    let version = text("marketingVersion")
        .or(text("version"))
        .ok_or("its newest version has no version")?;
    Ok(Release {
        link: link.to_string(),
        sha256: sha256.to_ascii_lowercase(),
        version: version.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Atelier's source on 13 Nov 2026, cut to what's read.
    const SOURCE_JSON: &str = r#"{
      "name": "Atelier",
      "apps": [{
        "name": "Atelier",
        "bundleIdentifier": "com.atelier.app",
        "versions": [{
          "version": "0.7.0",
          "marketingVersion": "0.7.0-alpha",
          "buildVersion": "1101",
          "downloadURL": "https://github.com/atelierstore/Atelier/releases/download/v0.7.0-alpha/Atelier-0.7.0-alpha.ipa",
          "size": 12792528,
          "sha256": "3917fbbf76e0ded3482accad8059bcf1fc215a58df0f818616c326a3ac565376",
          "minOSVersion": "17.0"
        }, {
          "version": "0.6.0",
          "downloadURL": "https://github.com/atelierstore/Atelier/releases/download/v0.6.0/Atelier-0.6.0.ipa",
          "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
        }]
      }]
    }"#;

    #[test]
    fn reads_the_newest_release() {
        assert_eq!(
            read(SOURCE_JSON.as_bytes()).unwrap(),
            Release {
                link: "https://github.com/atelierstore/Atelier/releases/download/v0.7.0-alpha/Atelier-0.7.0-alpha.ipa".into(),
                sha256: "3917fbbf76e0ded3482accad8059bcf1fc215a58df0f818616c326a3ac565376".into(),
                version: "0.7.0-alpha".into(),
            }
        );
    }

    #[test]
    fn takes_only_atelier_from_its_releases_with_a_sha256() {
        let changed = |from: &str, to: &str| read(SOURCE_JSON.replacen(from, to, 1).as_bytes());
        assert!(changed("com.atelier.app", "com.other.app").is_err());
        assert!(
            changed(
                "https://github.com/atelierstore/Atelier/releases/download/v0.7.0-alpha/",
                "https://example.com/"
            )
            .is_err()
        );
        assert!(changed("download/v0.7.0-alpha/", "download/../../x/").is_err());
        assert!(changed("3917fbbf", "3917fbbz").is_err());
        assert!(changed("\"sha256\": \"3917", "\"sha\": \"3917").is_err());
        assert!(read(b"{}").is_err());
        assert!(read(b"not json").is_err());
        // Without a marketing version, the version.
        assert_eq!(
            changed("\"marketingVersion\": \"0.7.0-alpha\",", "")
                .unwrap()
                .version,
            "0.7.0"
        );
    }
}
