//! On WASI, the program's calls to wasi-libc's `__wasi_fd_readdir` go to
//! its own (src/listing.rs), which lists a folder to the end under
//! rootshell. The program's alone, and its unit tests': the examples and
//! the tests in tests/ are crates of their own, without it.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        println!("cargo:rustc-link-arg-bins=--wrap=__wasi_fd_readdir");
    }
}
