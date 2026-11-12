# apple-codesign-quick, for rootshell

[apple-codesign-quick](https://github.com/Dadoum/apple-crates) 0.1.0, LGPL-2.1-or-later, as published on crates.io, with the change below, for `atelier-install` (`../..`), which signs with it through its copy of isideload (`../isideload`). The published manifest is `Cargo.toml.orig`; `Cargo.toml` is as published too. The published crate carries no license text: `LICENSE` is the LGPL 2.1's, from gnu.org.

rootshell runs a program in a web view that isn't in any window, and WebKit can suspend such a view's work, as runs on an iPhone show it does (`../../src/awake.rs`). What a program asks of rootshell wakes it, but the signing's hashing asks nothing for as long as it takes: on an iPhone, signing Atelier sat at 50%, hashing its 38 MB binary, until Return was pressed. So:

- **Check-ins** (`src/lib.rs`): `set_check_in` sets a function the signing calls as it hashes, which `atelier-install` checks in with rootshell from. Without one, nothing's called.
- **Code pages** (`src/signature.rs`, `hash_code_pages`): a check-in before each 64 pages, 256 KB.
- **Files** (`src/bundle.rs`, `hash_file_data`): a file of 1 MB or more is hashed a megabyte at a time, with a check-in before each, to the same digests.

Each changed file says so at its top. Everything else is as published. Signed with it under rootshell's runtime (`../../examples/sign.rs`), Atelier's IPA comes out byte for byte as the published crate signs it.

It isn't a member of the workspace, so `cargo fmt --all` leaves it as published; `atelier-install` signs with the published crate, through isideload from crates.io.
