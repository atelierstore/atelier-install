# Security

atelier-install holds an Apple ID's password, Apple's token for it, a pairing that opens the iPhone's install and file services, and the key Apple makes a development certificate for. A flaw in it could leak them or install something you didn't choose, so please report one privately.

## Reporting a vulnerability

Use **Report a vulnerability** on this repo's [Security tab](https://github.com/atelierstore/atelier-install/security). It's GitHub's private vulnerability reporting, so only the maintainers see your report. Please don't open an issue or a pull request for it.

Say what you found, how to make it happen, and which version you ran: the first line of `atelier-install.log`, in rootshell's folder, names it. We'll reply on the report, fix it in a new release, and publish an advisory once the fix is out. We'll credit you in the advisory, unless you'd rather not be named.

## Supported versions

Only the newest release, the one the README's download link serves. Fixes come as a new release, not as changes to an old one.

## What to report

- Anything that sends the Apple ID, its password, Apple's token, a two-factor code, the pairing or the signing key anywhere but where the README says they go: Apple, and the app being installed when it takes them.
- Anything that hands an app the pairing or the signing identity without your yes, apart from what the README says Atelier gets without being asked.
- Anything that lets an IPA, a server or another app run code in the program, or makes it write anywhere in an app's container but the paths the app declares.
- Anything that stays in `.atelier` after the README says it goes, or a secret written to `atelier-install.log`.
- A release whose `atelier-install.wasm` doesn't match its SHA-256 or its build provenance.

These aren't vulnerabilities in atelier-install:

- What an anisette server sees. It provides the machine identity Apple expects, which the README describes.
- What an app does once it's installed. The program installs the IPA you give it, so check it against its SHA-256 with `--sha256`.
- Flaws in iOS, rootshell, LocalDevVPN or Apple's services themselves. Report those to their makers, unless the program uses them in a way that makes things worse.

## Checking a download

Each release lists `atelier-install.wasm`'s SHA-256, and GitHub keeps signed build provenance for it, which says that this repo's release workflow built it, from which commit. To check it with GitHub's CLI:

```sh
gh attestation verify atelier-install.wasm --repo atelierstore/atelier-install
```
