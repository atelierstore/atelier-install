# Third-party notices

atelier-install is GPL-3.0-only (`LICENSE`). It's built from these crates, which keep their own licenses.

## Vendored, with changes

Each folder's README says what was changed, and each changed file says so at its top.

| Crate | Version | By | License | Where |
|---|---|---|---|---|
| [isideload](https://github.com/nab138/isideload) | 0.5.2 | Nicholas Sharp (nab138) | MIT | `vendor/isideload` |
| [jktcp](https://github.com/jkcoxson/jktcp) | 0.1.8 | Jackson Coxson | MIT | `vendor/jktcp` |
| [idevice](https://github.com/jkcoxson/idevice) | 0.1.68 | Jackson Coxson | MIT | `vendor/idevice` |
| [apple-codesign-quick](https://github.com/Dadoum/apple-crates) | 0.1.0 | Dadoum | LGPL-2.1-or-later | `vendor/apple-codesign-quick` |

apple-codesign-quick's source, with its changes, is in this repository, and so is the source of the whole program. You can change the library and build the program again with it, as the LGPL asks.

## From crates.io

- The other crates in `Cargo.lock` that go into the program are MIT, Apache-2.0 or both. The exceptions are curve25519-dalek, ed25519-dalek, x25519-dalek and subtle, which are BSD-3-Clause; rustls-webpki and untrusted, which are ISC; and zlib-rs, which is Zlib. A few more offer Unlicense, 0BSD, BSD-2-Clause or Unicode-3.0 as well. Each crate's license comes with it on crates.io, and `cargo metadata` lists them.

## Fonts

The page that `--page` serves uses Bricolage Grotesque (`src/page/display.woff2`, its Latin subset, as Atelier's website serves it), by The Bricolage Grotesque Project Authors. Its license is the SIL Open Font License 1.1, in `src/page/OFL.txt`.

## From Atelier

`src/anisette.json`, the list of anisette servers built into the program, is a copy of Atelier's (`Packages/AtelierKit/Sources/AppleIDKit/anisette.json`), GPL-3.0. So are the page's icon (`src/page/icon.png`) and its colors and buttons (`src/page/page.css`), from Atelier's website (`infra/website/public`).
