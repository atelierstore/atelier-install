use idevice::IdeviceError;
use rootcause::prelude::*;

pub mod anisette;
pub mod auth;
pub mod dev;
pub mod http;
pub mod sideload;
pub mod util;

#[derive(Debug, thiserror::Error)]
pub enum SideloadError {
    #[error("Auth error {0}: {1}")]
    AuthWithMessage(i64, String),

    #[error("Plist parse error: {0}")]
    PlistParseError(String),

    #[error("Developer error {0}: {1}")]
    DeveloperError(i64, String),

    #[error("Invalid bundle: {0}")]
    InvalidBundle(String),

    #[error("{0}")]
    IdeviceError(#[from] IdeviceError),
}

/// What the published crate installs here is a formatter for reqwest's
/// errors, which this copy doesn't use: its own HTTP client's errors say
/// what went wrong themselves. Kept so callers stay the same.
pub fn init() -> Result<(), Report> {
    Ok(())
}
