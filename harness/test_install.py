#!/usr/bin/env python3
"""atelier-install.wasm, as far as it goes without an iPhone.

Runs the real atelier-install.wasm under rootshell.py and plays the rest:
reads its announcement with dnslib, pairs the stand-in iPhone (`testphone`)
with it on the code it shows, and presses Return where it asks the person to
connect LocalDevVPN. There's no iPhone service behind 10.7.0.1 here, so each
run stops at the tunnel, and says what to do.

Then checks that the pairing it keeps is the one the phone made and isn't
in the folder Files shows, that a second run uses it without pairing, that
a run that stops leaves an install it signed for the next run, and nothing
else in its work folder, that it takes and hides the file Atelier's pairing
program leaves, that it refuses one that isn't a pairing, and that --forget
deletes what it kept.

    cargo build --release --target wasm32-wasip1
    cargo build -p atelier-testphone
    python3 harness/test_install.py

Needs `pip install wasmtime dnslib`.
"""

import os
import plistlib
import queue
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

import dnslib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
WASM = os.path.join(ROOT, "target/wasm32-wasip1/release/atelier-install.wasm")
PHONE = os.path.join(ROOT, "target/debug/testphone")
SERVICE = "_remotepairing-pairable-host._tcp.local."


def this_address():
    """An address of this machine that isn't the loopback, as the iPhone's
    Wi-Fi address is."""
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("192.0.2.1", 9))
        return probe.getsockname()[0]
    finally:
        probe.close()


def closed_port():
    """A port nothing listens on, where the pairing service isn't."""
    probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]
    probe.close()
    return port


class Program:
    """atelier-install.wasm in rootshell.py, and what it has printed."""

    def __init__(self, folder, *arguments, mdns_port=None):
        mdns = ["--mdns", f"127.0.0.1:{mdns_port}"] if mdns_port else []
        self.process = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "rootshell.py"), "--dir", folder,
             *mdns, "--device", "127.0.0.1", WASM, *arguments],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )
        self.lines = queue.Queue()
        self.printed = []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def wait_for(self, pattern, what):
        """The first line from now on that matches, as its match."""
        while True:
            try:
                line = self.lines.get(timeout=30)
            except queue.Empty:
                fail(f"the program never printed {what}", self)
            if line is None:
                fail(f"the program ended before printing {what}", self)
            self.printed.append(line)
            found = re.search(pattern, line)
            if found:
                return found

    def type(self, line):
        self.process.stdin.write(line + "\n")
        self.process.stdin.flush()

    def end(self):
        """Its exit code and the rest of what it printed."""
        status = self.process.wait(timeout=60)
        while (line := self.lines.get(timeout=30)) is not None:
            self.printed.append(line)
        return status, "".join(self.printed)


def run(folder, *arguments, typed=""):
    """A run to its end, with what's typed: its exit code and output."""
    program = Program(folder, *arguments)
    program.process.stdin.write(typed)
    program.process.stdin.close()
    return program.end()


def fail(why, program=None):
    if program is not None:
        sys.stderr.write("".join(program.printed))
        program.process.kill()
    sys.exit(f"FAILED: {why}")


def check(condition, why, program=None):
    if not condition:
        fail(why, program)


def main():
    for needed in (WASM, PHONE):
        check(os.path.exists(needed), f"{needed} isn't built (see this file's top)")
    address = this_address()
    port = str(closed_port())
    folder = tempfile.mkdtemp(prefix="atelier-install-")
    with open(os.path.join(folder, "ifconfig.txt"), "w") as ifconfig:
        ifconfig.write("en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n"
                       f"\tinet {address} netmask 0xffffff00 broadcast 192.168.1.255\n")
    kept = os.path.join(folder, ".atelier", "pairing.plist")
    offered = os.path.join(folder, "atelier-pairing.plist")

    # No pairing yet: it announces a pairable host, as the pairing program
    # does, and pairs on the code it shows.
    observer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    observer.bind(("127.0.0.1", 0))
    observer.settimeout(30)
    program = Program(folder, "--check", "--port", port, mdns_port=observer.getsockname()[1])
    program.wait_for(r"isn't paired with this program yet", "that it pairs")
    packet, _ = observer.recvfrom(4096)
    record = dnslib.DNSRecord.parse(packet)
    srv = next(a for a in record.rr if dnslib.QTYPE[a.rtype] == "SRV")
    a = next(a for a in record.rr if dnslib.QTYPE[a.rtype] == "A")
    instance = next(str(a.rdata) for a in record.rr
                    if dnslib.QTYPE[a.rtype] == "PTR" and str(a.rname) == SERVICE)
    check(str(a.rdata) == address, f"announced {a.rdata}, not {address}", program)
    program.wait_for(r"Pair with Atelier", "what to do in Settings")
    phone = subprocess.Popen([PHONE, "pair", address, str(srv.rdata.port)], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    check(phone.stdout.readline().strip() == "code?", "the phone didn't get to the code", program)
    code = "".join(program.wait_for(r"Code for Settings: (\d{3}) (\d{3})", "the code").group(1, 2))
    phone.stdin.write(code + "\n")
    phone.stdin.flush()
    stored = dict(line.split(" ", 1) for line in phone.stdout.read().splitlines())
    check(phone.wait(timeout=30) == 0, f"the phone didn't pair: {phone.stderr.read()}", program)
    check(stored["identifier"] == instance.split(".")[0], "the phone stored another host", program)
    program.wait_for(r"Paired with Test iPhone \(iPhone16,1\)", "that it paired")
    print(f"ok  it announced itself at {address}:{srv.rdata.port} and paired on the code it showed")

    # Then it has the person connect LocalDevVPN, and goes on to the tunnel.
    program.wait_for(r"open LocalDevVPN and tap Connect", "to connect LocalDevVPN")
    program.type("")
    program.wait_for(rf"at port {port}", "where it connects")
    status, said = program.end()
    check(status == 1 and f"pairing service didn't answer at port {port}" in said,
          f"without an iPhone, it said ({status}):\n{said}")
    check("press Control-C" in said, f"it didn't say how to stop a connection that waits:\n{said}")
    # In Settings, the person sees notifications, not the terminal.
    check(re.search(r"\x1b\]777;notify;Atelier;Code for Settings: \d{3} \d{3}\x07", said)
          and "\x1b]777;notify;Atelier;Paired. Open LocalDevVPN" in said,
          f"it didn't post the code, and the pairing, as notifications:\n{said!r}")
    print("ok  it waits for LocalDevVPN, then stops at the tunnel without an iPhone, and says what to do")
    print("ok  it posts the code, and what to do once paired, as notifications")

    # The pairing is the phone's, and only kept where Files doesn't show it.
    with open(kept, "rb") as file:
        pairing = plistlib.load(file)
    check(pairing["identifier"] == stored["identifier"], "the kept pairing names another host")
    check(pairing["public_key"].hex() == stored["public_key"], "the kept pairing's key isn't the one the phone stored")
    check(not os.path.exists(offered), "a pairing file in the folder Files shows")
    check(not os.path.exists(os.path.join(folder, ".atelier", "port")), "it kept a port that didn't work")
    with open(os.path.join(folder, "atelier-install.log")) as log:
        check("[2/2] This iPhone, through LocalDevVPN" in log.read(), "the log doesn't hold the run's steps")
    print("ok  it keeps the pairing the phone made in .atelier, and the run's steps in its log")

    # A second run uses it, without announcing anything.
    status, said = run(folder, "--check", "--port", port)
    check(status == 1 and "Using the pairing kept from the last run" in said
          and "Pair with Atelier" not in said, f"the second run said ({status}):\n{said}")
    print("ok  a second run uses the kept pairing")

    # An install a run signed and didn't finish waits for the next run,
    # whatever stops this one: its record, its sign-in and its signed app.
    state = os.path.join(folder, ".atelier")
    app = os.path.join(state, "work", "Atelier.ipa_extracted", "Payload", "Atelier.app")
    os.makedirs(app)
    with open(os.path.join(app, "Atelier"), "wb") as file:
        file.write(b"signed")
    with open(os.path.join(state, "install.plist"), "wb") as file:
        plistlib.dump({
            "Source": "https://example.com/Atelier.ipa", "SHA256": "ab" * 32,
            "IPABundleID": "com.atelier.app", "BundleID": "com.atelier.app.TEAM",
            "Name": "Atelier", "Version": "0.6.0",
            "App": "/.atelier/work/Atelier.ipa_extracted/Payload/Atelier.app",
            "SigningTo": "Library/Caches/Installer/certificate.plist",
            "SignedAt": int(time.time()),
        }, file)
    with open(os.path.join(state, "install-signing.plist"), "wb") as file:
        file.write(b"sign-in")
    status, said = run(folder, "--check", "--port", port)
    check(status == 1 and os.path.isfile(os.path.join(app, "Atelier"))
          and os.path.exists(os.path.join(state, "install.plist"))
          and os.path.exists(os.path.join(state, "install-signing.plist")),
          f"a run that stopped didn't leave the install for the next ({status}):\n{said}")
    print("ok  a run that stops leaves the install it signed for the next run")

    # Without one, the work folder goes at the end of a run, and so does a
    # sign-in left without its record.
    os.remove(os.path.join(state, "install.plist"))
    status, said = run(folder, "--check", "--port", port)
    check(status == 1 and not os.path.exists(os.path.join(state, "work"))
          and not os.path.exists(os.path.join(state, "install-signing.plist")),
          f"a run left its work folder, or a sign-in, with no install waiting:\n{said}")
    print("ok  otherwise a run deletes its work folder, and any sign-in left")

    # The pairing program's file: taken, and gone from view.
    shutil.move(kept, offered)
    status, said = run(folder, "--check", "--port", port)
    check("Took atelier-pairing.plist from the pairing program" in said, f"it didn't take the file:\n{said}")
    check(os.path.exists(kept) and not os.path.exists(offered), "the pairing program's file wasn't moved")
    print("ok  it takes the pairing program's file, and leaves none in view")

    # One that isn't a pairing: refused, with what to do.
    os.remove(kept)
    with open(offered, "w") as file:
        file.write("not a pairing")
    status, said = run(folder, "--check", "--port", port)
    check(status == 1 and "isn't a pairing file" in said, f"with a bad file, it said ({status}):\n{said}")
    os.remove(offered)
    print("ok  it refuses a file that isn't a pairing")

    # Nothing typed for the Apple ID: it stops before anything goes out.
    status, said = run(folder, "--sign-in", typed="\n")
    check(status == 1 and "no Apple ID was given" in said, f"--sign-in with nothing typed said ({status}):\n{said}")
    print("ok  --sign-in stops when no Apple ID is typed")

    # --forget deletes what it kept.
    status, said = run(folder, "--forget")
    check(status == 0 and not os.path.exists(os.path.join(folder, ".atelier")), f"--forget left .atelier:\n{said}")
    print("ok  --forget deletes what it kept")
    shutil.rmtree(folder)
    print("PASSED")


if __name__ == "__main__":
    main()
