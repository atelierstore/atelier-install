# isideload, for rootshell

[isideload](https://github.com/nab138/isideload) 0.5.2 by Nicholas Sharp (nab138), MIT (`LICENSE`), as published on crates.io, with the changes below so that it runs as a WASI Preview 1 program in rootshell, for `atelier-install` (`../..`). The published manifest is `Cargo.toml.orig`; `Cargo.toml` is this copy's.

rootshell's programs have one thread and no async runtime, and rootshell does TLS and name lookups for them (`rootshell_socket_tls_connect_host`). reqwest and tokio-tungstenite need tokio's sockets, which WASI Preview 1 doesn't have, and the browser build's replacements need a browser. So:

- **HTTP** (`src/http.rs`, new): a small HTTP/1.1 client with reqwest's API as isideload uses it (`Client`, `RequestBuilder`, `Response`, the `http` crate's headers), on a connection the program hands it with `isideload::http::set_connector`: rootshell's TLS for `atelier-install`. One request per connection, redirects followed as reqwest follows them, bodies by length, chunked or to the end, gzipped or deflated as the server says: it asks for nothing compressed, but Apple's developer services gzip their answers all the same. Without a connector, plain HTTP goes through the system's sockets and HTTPS fails.
- **The anisette servers' WebSocket** (`src/anisette/remote_v3/websocket.rs`): tungstenite's blocking WebSocket on a connection from the same connector, in place of tokio-tungstenite and ws_stream_wasm.
- **Gone**: reqwest, reqwest-middleware and its proxy middleware (`src/auth/middleware.rs`), rustls, the keyring storage (`src/util/keyring_storage.rs`), the `tokio-mt` feature, and reqwest's error formatter in `init()`. A proxy, for the web build, still prefixes each link.
- **Unpacking the IPA** (`src/sideload/application.rs`): zip's `extract` only knows directory entries on Unix and Windows, and on WASI makes a file of each, so the next entry inside one fails. This copy unpacks entry by entry through isideload's VFS, refuses symbolic links and entries that would land outside the folder, and writes a megabyte at a time.
- **Uploads**: AFC chunks of 1 MB, as on any computer, unless the `wasm` feature (a browser's) asks for the small ones.
- **Signing** (`Cargo.toml`): apple-codesign-quick from `../apple-codesign-quick`, which checks in with rootshell as it hashes (see its README).
- **Tests** for the HTTP client, the WebSocket and the unpacking.

Everything else is as published. On WASI, apple-codesign-quick's rayon runs on the one thread, and its memmap2 falls back to reading the file. Signed under wasmtime, Atelier's IPA comes out byte for byte as signed natively.

```sh
cargo test --manifest-path vendor/isideload/Cargo.toml   # from the workspace's root
```

It's a member of the workspace, for its tests.
