//! What the tests share: a folder of their own to write in. WASI has no
//! temporary folder and no process ID, so there it's a folder in the one the
//! tests run in, named at random.

use std::path::PathBuf;

/// A new, empty folder for the test `name`.
pub fn scratch(name: &str) -> PathBuf {
    #[cfg(target_os = "wasi")]
    let base = PathBuf::from("atelier-install-tests");
    #[cfg(not(target_os = "wasi"))]
    let base = std::env::temp_dir().join("atelier-install-tests");
    let mut random = [0u8; 4];
    getrandom::getrandom(&mut random).expect("the system's random source");
    let folder = base.join(format!("{name}-{}", hex::encode(random)));
    std::fs::create_dir_all(&folder).unwrap();
    folder
}
