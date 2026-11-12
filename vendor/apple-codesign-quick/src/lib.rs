// Changed for atelier-install on 12 November 2026: `set_check_in`, which the
// signing calls as it hashes (README.md).

pub mod bundle;
pub mod error;
mod file_bytes;
pub mod macho;
pub mod provisioning;
pub mod rustcrypto_cms;
pub mod signature;

pub use bundle::{Bundle, BundleSigningSettings, CodeResourcesMaps, sign_bundle};
pub use error::{CodeSignError, Result};
pub use macho::{
    DEFAULT_CMS_BLOB_RESERVATION, MachOSigningConfig, sign_macho_data, sign_macho_file,
};
pub use provisioning::ProvisioningProfile;
pub use rustcrypto_cms::RustCryptoCmsSigner;
pub use signature::{CmsSigner, CmsSigningRequest, EncodedCodeDirectory, HashAlgorithm};

use std::sync::OnceLock;

static CHECK_IN: OnceLock<fn()> = OnceLock::new();

/// Has the signing call `check_in` every few hundred kilobytes it hashes,
/// for a program whose host stops it when it hears nothing from it for a
/// while, as iOS does rootshell's: the hashing calls nothing else. The
/// first one set is the one called.
pub fn set_check_in(check_in: fn()) {
    let _ = CHECK_IN.set(check_in);
}

pub(crate) fn check_in() {
    if let Some(check_in) = CHECK_IN.get() {
        check_in();
    }
}
