# atelier-install

atelier-install installs an app on the iPhone it runs on, signed with your free Apple ID, with nothing but App Store apps on that iPhone. It's one WebAssembly program of about 3 MB that runs in [rootshell](https://apps.apple.com/app/rootshell-local-terminal-ssh/id6755794662), a terminal app on the App Store. It pairs the iPhone with itself, reaches the iPhone's own services through LocalDevVPN, another App Store app, signs in to Apple, signs the app, and installs it as Xcode installs a developer's app. It was made to install [Atelier](https://github.com/atelierstore/Atelier), and it installs any IPA.

## What it solves

To install an app from outside the App Store with a free Apple ID, you need three things. Until now, an iPhone couldn't provide the first one by itself.

1. **A pairing with the iPhone.** iOS's install and file services only answer a host the iPhone has paired with. Before iOS 27, pairing took a computer, over USB or the network. So every sideloading tool needed a computer at least once, to make a pairing file.
2. **A signature that Apple's developer services issue.** You need a development certificate in the Apple ID's team, the iPhone registered in that team, an App ID and a provisioning profile, as Xcode gets them by signing in to Apple. That sign-in also needs anisette data, the machine identity Apple expects from a Mac. Free tools get it from anisette servers.
3. **A way to reach the iPhone's services from the iPhone itself.** iOS's services aren't on a socket an app can connect to. LocalDevVPN routes 10.7.0.1 to them, the way SideStore and StikDebug reach them.

So the first sideloaded app has always needed something more:

- **A computer**, to pair the iPhone or to do the first install, as with AltStore, SideStore, Sideloadly and iloader.
- **An app installed with someone else's certificate**, such as SideInstaller from its website. Apple revokes those certificates.
- **Since iOS 27, two App Store apps working together.** siboot pairs from rootshell, then signs and installs from Open Minis, an app that runs Linux programs. This project's first version worked the same way.

## What's new

Every step runs in one App Store app, as one program. There's no computer at any point, no certificate but your own, and no second app.

- **The iPhone pairs with itself from inside rootshell's sandbox.** The program sends iOS 27's Bonjour announcement for a pairable host as an mDNS packet it builds itself.
- **The whole stack runs as WASI Preview 1:** idevice for the iPhone's services, and isideload for Apple's sign-in and the signing. That means one thread and no async runtime, with sockets and TLS lent by rootshell.
- **It keeps working while the web view it runs in could be suspended.** It checks in with rootshell as it computes.
- **A run that stops picks up where it left off,** and the next sign-in uses Apple's token instead of the password, except for an app that takes the password.
- **An app can take Apple's token in place of the password,** so that the password only goes to Apple.
- **Apps declare in their Info.plist what they take from it,** such as the pairing or the signing identity. It hands over nothing else, and runs nothing that comes with the IPA.

## How it works

1. **Pairing** (`src/pairing.rs`, `src/mdns.rs`, `src/address.rs`). On iOS 27, Settings › Privacy & Security › Developer Mode lists a host as "Pair with …" when it announces `_remotepairing-pairable-host._tcp` over Bonjour. The iPhone then connects to that host and runs pair-setup with a 6-digit code it shows. An app may only announce the Bonjour types its Info.plist declares. But rootshell has Apple's multicast entitlement, so a program it runs can send the announcement itself, through rootshell's UDP socket calls.
   - The program builds the mDNS response by hand: a PTR record, an SRV record with the port it listens on, a TXT record with the keys of idevice's `PairableHostInfo`, and an A record with the iPhone's Wi-Fi address.
   - rootshell doesn't tell its programs that address, so the program reads it from the output of `ifconfig en0`.
   - It sends the response three times to 224.0.0.251:5353, with a 75-minute TTL.
   - When the iPhone connects, idevice's `PairableHost` runs pair-setup: SRP with the code, then the exchange of Ed25519 identities. The result is an RPPairing file, which the program keeps.
2. **The tunnel** (`src/phone.rs`, `src/tunnel.rs`).
   - The program connects to iOS's RemotePairing service at 10.7.0.1 through LocalDevVPN, on port 49152 or the port that worked last.
   - It pair-verifies with the RPPairing file, asks the iPhone for a tunnel listener, connects to it, and runs the TLS-PSK handshake that opens a CDTunnel, which carries IP packets.
   - Over the CDTunnel, a userspace TCP stack (jktcp) reaches RemoteServiceDiscovery, which lists the iPhone's services: lockdownd for its name and UDID, AFC, installation_proxy and house_arrest.
3. **The IPA** (`src/ipa.rs`). It comes from rootshell's folder, a path or an HTTPS link, and is checked against its SHA-256 when you give one. Its Info.plist gives the bundle ID, the name, the version, and what the app takes.
4. **The sign-in** (`src/apple.rs`, `src/route.rs`, `src/token.rs`).
   - The program signs in with GrandSlam's SRP, using the Apple ID and password, and Apple's two-factor code when Apple asks for one.
   - It goes through an anisette v3 server from Atelier's list. Each server's provisioned state is kept, so Apple sees the same machine every time.
   - It then gets the app token for Xcode's developer services, `com.apple.gs.xcode.auth`. The next run signs in with that token, unless the app it installs takes the password.
5. **The signing** (`src/apple.rs`).
   - Through Xcode's developer services, the program gets the team, registers the iPhone by its UDID, and gets a development certificate (a new one, or one it made before, with the key it kept). It then gets the app's App ID and its provisioning profile, which the app's extensions are signed with too, as Atelier signs them.
   - isideload then signs the bundle with apple-codesign-quick: the code directories, the entitlements from the profile, and a CMS signature with the certificate.
6. **The install** (`src/phone.rs`). AFC uploads the signed bundle to `PublicStaging`, and installation_proxy installs it with `PackageType` `Developer`, as Xcode does.
7. **The hand-off** (`src/handoff.rs`, `src/certificate.rs`). house_arrest opens the app's container, and the program writes what the app's Info.plist declares, at the paths it declares: this iPhone's pairing, the signing identity (the certificate, its key and the Apple ID's sign-in, with the password or Apple's token), or both.

## The tricks that made it work

- **rootshell's multicast entitlement.** It lets a program inside an App Store app announce a Bonjour type the app doesn't declare. With iOS 27's self-pairing, that removes the need for a computer.
- **An announcement that carries everything.** rootshell's programs do one blocking thing at a time, so the program can't answer mDNS queries while it waits for the iPhone to connect. Instead it sends unsolicited responses with every record and a long TTL, and the iPhone answers Settings from its cache.
- **Async code without a runtime.** WASI Preview 1 has one thread and blocking sockets. A blocking socket is an async one that's always ready, so the program wraps rootshell's sockets in tokio's I/O traits and polls each future to its end with a no-op waker (`src/blocking.rs`). jktcp normally needs a task and timers, so its copy is patched to be driven one step at a time (`vendor/jktcp`).
- **rootshell's TLS.** rootshell opens TLS connections for its programs with Network.framework and Apple's trust store (`rootshell_socket_tls_connect_host`). A small HTTP/1.1 client on those connections replaces isideload's reqwest (`vendor/isideload/src/http.rs`), so the program carries no TLS library and no root certificates. It unpacks gzip and deflate, because Apple's developer services gzip their answers even when the request doesn't ask for it.
- **Reads that fit rootshell's 64 KB slot.** rootshell hands back what a read brought through one 64 KB slot, as base64 in JSON, and fails a read that doesn't fit with EINVAL. The program defines its own `read`, `readv`, `write` and `writev`, which std and every crate link to in place of wasi-libc's. Reads ask for 32 KB at most, and writes go in pieces of 256 KB (`src/chunks.rs`, `src/net.rs`).
- **POSIX errors in place of FileManager's.** rootshell's file broker answers EIO for any folder creation, rename or delete it can't do. The program's own versions of those calls look at what's there and answer EEXIST or ENOENT, as std expects (`src/paths.rs`).
- **Check-ins.** rootshell runs its programs in a web view that isn't in any window, and WebKit can suspend such a view's work (`WKPreferences.inactiveSchedulingPolicy`). On an iPhone, a computation that asked rootshell nothing, hashing a 38 MB binary, stopped until a key was pressed. So at most every 50 ms, the program makes a call to rootshell that changes nothing (`fd_prestat_get`). It does so from the signing's hashing, through a patched copy of apple-codesign-quick, from the download, from the install, and from every 64th allocation (`src/awake.rs`).
- **Connections that never time out.** Network.framework keeps a connection waiting for a server that doesn't answer. So the program notes what it's waiting for, and the run after a Control-C tries that server last. It also goes straight to ports it knows instead of scanning for one.
- **The same machine to Apple.** The anisette state is kept for each server, and handed to the app along with the sign-in, so Apple doesn't ask for a code again.
- **Staying alive in Settings.** rootshell's Location Diary Mode keeps it running in the background while you type the code in Settings. It has to be Session Only: Auto During Active Sessions doesn't count a WebAssembly program as a session, so it stays off while the program runs. rootshell doesn't show the program's notifications (OSC 777) from the background, for the same reason: it keeps them until it's in front again, where it skips them. So the pairing's code is read in rootshell itself.

## Using it

You need:

- iOS 27 or later, with Developer Mode on (Settings › Privacy & Security › Developer Mode).
- Wi-Fi. iOS refuses LocalDevVPN's connections to its services over mobile data.
- LocalDevVPN, from the App Store.
- rootshell, from the App Store, with Location Diary Mode on and set to Session Only, in its Settings › General. Session Only goes off when rootshell closes, so turn it on again before a later run.
- An Apple ID. With a free one, apps open for 7 days.

In rootshell, run this line:

```sh
true && curl -fsSLO https://github.com/atelierstore/atelier-install/releases/latest/download/atelier-install.wasm && ifconfig en0 > ifconfig.txt && ./atelier-install.wasm --ipa https://example.com/App.ipa --sha256 <the IPA's SHA-256>
```

It downloads the program's latest release, saves the iPhone's Wi-Fi address in `ifconfig.txt` for pairing, and runs the program. It starts with `true` so that rootshell runs the whole line itself: rootshell starts a `.wasm` program after `&&` only when the line starts with one of its own commands, such as `true`. To install Atelier, [atelierstore.app/apple-id](https://atelierstore.app/apple-id) fills in this line with Atelier's newest release and its SHA-256.

Later, `./atelier-install.wasm` and its options alone run the program again. You only need `ifconfig.txt` the first time, to pair. Without `--ipa`, the program installs the IPA in rootshell's folder.

When it pairs, tap *Pair with Atelier* in Settings › Privacy & Security › Developer Mode, and enter your passcode. Settings then asks for a code: switch to rootshell to read it, then go back to Settings and type it. Then connect LocalDevVPN and come back to rootshell. The program says what the install takes from your Apple ID, such as a certificate, and asks before it goes on. At Apple's limit of certificates, it asks which one to revoke. It never revokes one by itself. Stay in rootshell while it installs. Then trust your Apple ID in Settings › General › VPN & Device Management, and open the app. This build doesn't renew it: run it again before the 7 days are over.

```
./atelier-install.wasm                  install the IPA in rootshell's folder
./atelier-install.wasm --ipa <where>    install the IPA at a path or an https link
./atelier-install.wasm --check          only pair, and check LocalDevVPN
./atelier-install.wasm --sign-in        only check that Apple takes your Apple ID

  --sha256 <hex>      the IPA's SHA-256, to check it against
  --address <ip>      this iPhone's Wi-Fi address, for pairing, in place of ifconfig.txt
  --port <number>     the iPhone's pairing service's port, when it isn't 49152
  --anisette <link>   your own anisette server, in place of Atelier's list
  --pairing-to <path> where the app takes this iPhone's pairing, in its container,
                      when its Info.plist doesn't say (it's asked)
  --redo              sign the IPA anew, where a run that stopped would go on
  --other-apple-id    ask for the Apple ID, where the program would sign in
                      with the one it kept from the last sign-in
  --forget            delete what this program keeps in .atelier
```

**A run that stops picks up where it left off.** What each step makes is kept until the install is done. That covers the download and, from the signing on, the signed app and what the app takes. A run for the same IPA, by its link or its SHA-256, goes straight to the install. If the install's connection breaks, which can happen while rootshell isn't on screen, the program asks to try again. Everything kept goes once the app is installed, a day after the signing, with `--redo`, or with `--forget`. If nothing more shows within a minute, a server isn't answering. Press Control-C and run it again, and it tries that server last.

**If step 2 shows nothing for a minute** while LocalDevVPN is connected, the iPhone's pairing service has likely moved to another port, which can happen after a restart. Press Control-C. A Bonjour browser on the iPhone, such as Discovery – DNS-SD Browser, shows the port under `_remotepairing._tcp`. Run the program again with `--port` and that number. It keeps the port for later runs.

## Your Apple ID

Everything stays on the iPhone. The password goes to Apple, and to the app being installed only when that app takes the sign-in with the password, in version 1 of the hand-off (see [what your app takes](#for-app-developers-what-your-app-takes)). An app that takes version 2 gets Apple's token for the sign-in instead, and the password only goes to Apple. The program says which before you type the password. Atelier takes the sign-in without asking. Any other app gets it only once you've said yes.

After a sign-in, the program keeps Apple's token for its developer services, not the password, so that its next run signs in without the password or a code. The token is kept under a hash of the Apple ID, so its file doesn't name the Apple ID. When a token is kept for one Apple ID only, the next run signs in with it without asking for the Apple ID, unless the app it installs takes the sign-in, which names the Apple ID; `--other-apple-id` asks for one. The token is deleted when Apple refuses it, 15 minutes before Apple says it ends, or with `--forget`. `--sign-in` still asks for the password, because it checks it.

A run for an app that takes the password asks for it whenever it signs in, because the app takes it too. A run for an app that takes the token asks for the Apple ID, signs in with the kept token when Apple takes it, and hands the app that token. From the signing until the app has them, the Apple ID and the password or token wait in `.atelier` with the signed app, so that a run that stops can still hand them over. They're deleted once the app has them, a day after the signing, with `--redo`, or with `--forget`.

The program keeps the following in `.atelier`, in rootshell's folder, which Files doesn't show:

- the pairing, until the app takes it
- the pairing service's port
- the signing key that Apple made a certificate for
- Apple's token
- each anisette server's state
- an install that a run stopped before finishing, with the sign-in when the app takes it

`--forget` deletes all of it once you type yes, after saying that the next run then needs a new certificate, which at Apple's limit means revoking one. Each run's steps and errors go to `atelier-install.log` in rootshell's folder, never with the Apple ID, its password or a code.

## For app developers: what your app takes

An app declares in its Info.plist what it takes once it's installed, and where in its container:

```xml
<key>SideloadHandOff</key>
<dict>
    <key>Version</key>
    <integer>2</integer>
    <key>PairingFile</key>
    <string>Library/Application Support/pairingFile.plist</string>
    <key>SigningIdentity</key>
    <string>Library/Caches/Installer/certificate.plist</string>
</dict>
```

- **What it can declare.** Either entry can be left out, and each path must be inside the app's Documents, Library or tmp folder.
- **How it's written.** The program writes what's declared through house_arrest, and nothing else. Nothing that comes with an IPA runs in the program that holds the pairing and the sign-in.
- **Your yes comes first.** For any app but Atelier, the program first says what each item lets the app do, and asks.
- **An app that declares nothing gets nothing.** `--pairing-to` hands the pairing to an app that reads it somewhere without declaring it.

Atelier gets two things without being asked: the pairing at `Library/Application Support/Pairing/pairingFile.plist`, unless it declares another path, and the signing identity, as it declares it, or, from its builds before `SideloadHandOff`, at `Library/Caches/Installer/certificate.plist` in version 1 when its Info.plist says `AtelierInstallerHandOff` (version 1).

`Version` says which formats the app reads. The pairing is the same in both versions. The signing identity has the password in version 1, and Apple's token in its place in version 2. The program hands over either, so an app's builds from before version 2 keep getting what they read. Declare version 2 unless your app needs the password: the password then only goes to Apple. This program's builds before version 2 refuse a declaration of version 2, so an app that declares it gets nothing from them, except Atelier, which also says `AtelierInstallerHandOff` for them.

### The pairing

An RPPairing file, as idevice writes it (`RpPairingFile`): an XML plist with the identity this program paired as. It's what pairing through Developer Mode's "Pair with …" makes on iOS 27. With it, an app pair-verifies with iOS's RemotePairing service, at 10.7.0.1 through LocalDevVPN, which then opens a tunnel to the iPhone's services. It isn't the lockdown pairing file that a computer makes over USB, as iloader and jitterbugpair do for SideStore and StikDebug.

| Key | Type | What it is |
|---|---|---|
| `identifier` | string | The host's identifier, a UUID, by which the iPhone knows the pairing |
| `public_key` | data, 32 bytes | The host's Ed25519 public key |
| `private_key` | data, 32 bytes | The host's Ed25519 private key |
| `alt_irk` | data, 16 bytes | The iPhone's identity resolving key from the pairing, which recognizes its `_remotepairing._tcp` announcements. idevice reads the file without it. |

### The signing identity

The Apple ID's sign-in and the certificate the app was signed with, as an XML plist (`src/certificate.rs`). With it, the app signs in to Apple as you, as the same machine, so Apple doesn't ask for a code again, and signs with the same certificate. Atelier moves it on its first launch and keeps only Apple's token, in its Keychain: handed the password, it signs in with it once to get one. An app should do the same rather than leave the file in its container.

In both versions:

| Key | Type | What it is |
|---|---|---|
| `appleID` | string | The Apple ID, as typed, in lowercase |
| `teamID` | string | The Apple ID's team |
| `certificate` | data | The development certificate, DER |
| `privateKey` | data | Its RSA key, PKCS#8 DER |
| `machineName` | string | What Apple lists the certificate as made on |
| `machineID` | string | The machine ID Apple lists for the certificate |
| `serialNumber` | string | The certificate's serial number |
| `udid` | string | The iPhone's UDID, which the program registered in the team |
| `anisetteServer` | string | The URL of the anisette v3 server the sign-in went through |
| `anisetteState` | data | That server's provisioned state, as isideload keeps it: an XML plist with `keychain_identifier` (16 bytes, the machine's identity, from which its device ID and `X-Apple-I-MD-LU` come) and `adi_pb` (the provisioning, which the server's `get_headers` takes) |

In version 1, which has no `version` key:

| Key | Type | What it is |
|---|---|---|
| `password` | string | The Apple ID's password |

Version 2 has Apple's token for Xcode's developer services, `com.apple.gs.xcode.auth`, in the password's place: the one the program got when it signed in with the password, in this run or an earlier one. An app calls the developer services with it as Xcode does, with the token as `X-Apple-GS-Token`, the adsid as `X-Apple-I-Identity-Id`, and the anisette server's headers from `anisetteState`. Apple takes it until it ends, or until Apple refuses it, and then the app has to sign in again with the password. Keep it as you'd keep the password: it does all that the app does with Apple until then.

| Key | Type | What it is |
|---|---|---|
| `version` | integer | 2 |
| `adsid` | string | Apple's ID for the account, which goes with the token |
| `token` | string | The token |
| `tokenDuration` | integer | How long it lasts, in seconds, as Apple gave it |
| `tokenExpiry` | integer | When it ends, as Apple gave it, in a unit Apple doesn't say. The program reads it as milliseconds since 1970 when that's a time from 2001 on, then as seconds, and otherwise goes by `tokenReceived` and `tokenDuration` |
| `tokenReceived` | date | When the program got it |
| `clientInfo`, `userAgent` | string | What the sign-in sent Apple in place of the anisette server's client info and user agent, when it did: send the same with the token |

## Building and testing

`rust-toolchain.toml` pins the Rust version, with the WASI target, so local builds, CI and releases use the same one. A new version comes as a pull request from Dependabot.

```sh
rustup toolchain install                        # the pinned Rust, once
cargo build --release --target wasm32-wasip1    # target/wasm32-wasip1/release/atelier-install.wasm
cargo test --workspace                          # the unit tests
```

`harness/rootshell.py` runs a program under wasmtime with rootshell's calls answered by the system's sockets, the way rootshell answers them, including its 64 KB slot and its file broker's errors. The unit tests also run as WASI under it. `harness/test_install.py` runs the real program in it: it pairs with a stand-in iPhone (`testphone`) on the code it shows, waits for LocalDevVPN, stops at the tunnel and says what to do, and keeps what it should.

```sh
pip install --require-hashes -r harness/requirements.txt
CARGO_TARGET_WASM32_WASIP1_RUNNER="python3 $PWD/harness/rootshell.py --dir $(mktemp -d)" \
    cargo test --target wasm32-wasip1
cargo build -p atelier-testphone
python3 harness/test_install.py
```

`examples/sign.rs` downloads and signs an IPA as the program does, with a certificate and key of your own in place of Apple's. It's for timing the signing under rootshell's runtime.

## Releases

Merging a pull request that changes `[workspace.package] version` in `Cargo.toml` publishes a release (`.github/workflows/release.yml`). CI runs every test on the merge commit and builds the program, and that same file is published as the release's `atelier-install.wasm`, with its SHA-256 and signed build provenance, which says this repo's release workflow built it, from which commit. The workflow tags the commit with the version, such as `v0.2.0`. A version with a suffix, such as `0.2.0-beta.1`, makes a pre-release, which the download link skips. An admin can also push that tag on a commit in main, if the version has no release yet. To check a download with GitHub's CLI:

```sh
gh attestation verify atelier-install.wasm --repo atelierstore/atelier-install
```

To report a vulnerability, see `SECURITY.md`.

## License

GPL-3.0-only (`LICENSE`). The vendored crates keep their own licenses: isideload and jktcp are MIT, and apple-codesign-quick is LGPL-2.1-or-later. Each folder in `vendor` says what was changed. `THIRD_PARTY_NOTICES.md` lists them, and idevice, MIT, which comes from crates.io.
