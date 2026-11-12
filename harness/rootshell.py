#!/usr/bin/env python3
"""Runs a rootshell program on a computer, for the tests.

rootshell runs WASI Preview 1 programs and lends them sockets through calls
of its own (`rootshell_socket_*`, in its wasm/README.md), which it answers
with Apple's Network.framework. This runs a program under wasmtime with
those calls answered by the system's sockets, the way rootshell answers
them: fds from 100 up, WASI's error numbers, bind keeping only the port,
TLS done for the program by the runtime, a handshake that fails as a
refused connection, a read that fails when what it brings doesn't fit
rootshell's 64 KB reply slot, from a connection or a file, folders, renames
and deletes that fail as rootshell's FileManager makes them fail, and none
of the calls rootshell's runtime doesn't have (getsockname among them), so
a program that leans on one fails here as it would there. Its folder is
`/`, as rootshell's is, and its terminal calls (`rootshell_terminal_*`) are
there too.

    rootshell.py [--dir FOLDER] [--mdns HOST:PORT] [--device HOST]
        [--host NAME=HOST:PORT ...] [--ca FILE] program.wasm [arguments]

--dir is the folder the program sees as rootshell's (default: the current
one). --mdns sends what the program multicasts to 224.0.0.251:5353 to
HOST:PORT instead, where a test can read it. --device connects what the
program connects to 10.7.0.1, the iPhone through LocalDevVPN, to HOST
instead. --host connects what the program connects to by the name NAME to
HOST:PORT instead, with TLS still checked against NAME; --ca is the
certificate authority TLS trusts in place of the system's.

Needs `pip install wasmtime`.
"""

import argparse
import base64
import os
import socket
import ssl
import sys

import wasmtime

EBADF, EINVAL, EIO, EADDRINUSE = 8, 28, 29, 1
ECONNREFUSED, ENETUNREACH, ETIMEDOUT = 14, 50, 73
AF_INET, SOCK_STREAM, SOCK_DGRAM = 2, 1, 2
MDNS = ("224.0.0.251", 5353)
DEVICE = "10.7.0.1"
# rootshell hands each read's bytes back to the program through one 64 KB
# slot of shared memory, as JSON with the bytes in base64, behind an 8-byte
# header (Resources/Wasm/wasm-host.js, `writeReplyAndNotify`). A reply that
# doesn't fit fails with EINVAL, and the bytes it held are gone: the broker
# took them from the connection, or the file's position moved past them,
# before the reply was written.
REPLY_SLOT = 64 * 1024


def fits(length):
    """Whether a read that brought `length` bytes fits rootshell's slot:
    its base64, and the JSON around it (`{"errno":0,"data":"…","callID":
    "sk-123456"}`, 40 bytes or so, with room for a long call ID)."""
    return 4 * ((length + 2) // 3) + 64 + 8 <= REPLY_SLOT


# Calls WASI's own file calls for the program, with a memory of its own,
# where rootshell answers differently: the iovec at 0, the count at 8, a
# file's status at STAT_AT, paths at PATH_AT and TO_AT, a read's bytes from
# READ_AT.
HELPER = """
(module
  (import "wasi_snapshot_preview1" "fd_read"
    (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_filestat_get"
    (func $stat (param i32 i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_create_directory"
    (func $mkdir (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_unlink_file"
    (func $unlink (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_remove_directory"
    (func $rmdir (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_rename"
    (func $rename (param i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "read") (param $fd i32) (param $length i32) (result i32)
    (i32.store (i32.const 0) (i32.const 0x8100))
    (i32.store (i32.const 4) (local.get $length))
    (call $fd_read (local.get $fd) (i32.const 0) (i32.const 1) (i32.const 8)))
  (func (export "stat") (param $fd i32) (param $length i32) (result i32)
    (call $stat (local.get $fd) (i32.const 0) (i32.const 0x100) (local.get $length)
      (i32.const 16)))
  (func (export "mkdir") (param $fd i32) (param $length i32) (result i32)
    (call $mkdir (local.get $fd) (i32.const 0x100) (local.get $length)))
  (func (export "unlink") (param $fd i32) (param $length i32) (result i32)
    (call $unlink (local.get $fd) (i32.const 0x100) (local.get $length)))
  (func (export "rmdir") (param $fd i32) (param $length i32) (result i32)
    (call $rmdir (local.get $fd) (i32.const 0x100) (local.get $length)))
  (func (export "rename")
    (param $fd i32) (param $length i32) (param $to_fd i32) (param $to_length i32)
    (result i32)
    (call $rename (local.get $fd) (i32.const 0x100) (local.get $length)
      (local.get $to_fd) (i32.const 0x4100) (local.get $to_length))))
"""
STAT_AT, PATH_AT, TO_AT, READ_AT = 16, 0x100, 0x4100, 0x8100
LONGEST_PATH = TO_AT - PATH_AT


def parent(path):
    """The folder a relative path is in: `.` for the one it's relative to."""
    return path.rstrip(b"/").rpartition(b"/")[0] or b"."


def pending(sock, length):
    """What a connection has for the program, as rootshell's broker hands
    it over: once something has come, all that's there, up to `length`, as
    Network.framework gathers it, not one TLS record at a time."""
    data = sock.recv(length)
    if not data:
        return data
    sock.setblocking(False)
    try:
        while len(data) < length:
            more = sock.recv(length - len(data))
            if not more:
                break
            data += more
    except (BlockingIOError, ssl.SSLWantReadError):
        pass
    finally:
        sock.setblocking(True)
    return data


class Sockets:
    """rootshell's socket broker, on the system's sockets."""

    def __init__(self, mdns_to, device=None, hosts=None, ca=None):
        self.handles = {}
        self.next_fd = 100
        self.mdns_to = mdns_to
        self.device = device
        self.hosts = hosts or {}
        self.tls = ssl.create_default_context(cafile=ca)

    def add(self, handle):
        fd = self.next_fd
        self.next_fd += 1
        self.handles[fd] = handle
        return fd

    def socket(self, mem, domain, kind, fd_out):
        if domain != AF_INET or kind not in (SOCK_STREAM, SOCK_DGRAM):
            return EINVAL
        mem.i32(fd_out, self.add({"kind": kind, "port": 0, "sock": None}))
        return 0

    def bind(self, mem, fd, addr, addr_len):
        handle = self.handles.get(fd)
        parsed = mem.sockaddr(addr, addr_len)
        if handle is None:
            return EBADF
        if parsed is None:
            return EINVAL
        handle["port"] = parsed[1]
        return 0

    def listen(self, mem, fd, backlog):
        handle = self.handles.get(fd)
        if handle is None:
            return EBADF
        if handle["kind"] != SOCK_STREAM:
            return EINVAL
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            listener.bind(("0.0.0.0", handle["port"]))
            listener.listen(max(backlog, 1))
        except OSError:
            return EADDRINUSE
        handle["sock"] = listener
        handle["listening"] = True
        return 0

    def accept(self, mem, fd, fd_out):
        handle = self.handles.get(fd)
        if handle is None or not handle.get("listening"):
            return EBADF
        conn, _ = handle["sock"].accept()
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        mem.i32(fd_out, self.add({"kind": SOCK_STREAM, "port": 0, "sock": conn}))
        return 0

    def connect(self, mem, fd, addr, addr_len):
        handle = self.handles.get(fd)
        to = mem.sockaddr(addr, addr_len)
        if handle is None:
            return EBADF
        if handle["kind"] != SOCK_STREAM or handle["sock"] is not None or to is None:
            return EINVAL
        if to[0] == DEVICE and self.device:
            to = (self.device, to[1])
        return self._connect(handle, to)

    def connect_host(self, mem, fd, host, host_len, port):
        """A connection to a name, which rootshell looks up itself."""
        handle = self.handles.get(fd)
        if handle is None:
            return EBADF
        if handle["kind"] != SOCK_STREAM or handle["sock"] is not None:
            return EINVAL
        name = mem.read(host, host_len).decode()
        return self._connect(handle, self.hosts.get(name, (name, port)))

    def tls_connect_host(self, mem, fd, host, host_len, port):
        """The same with TLS, which rootshell does for the program: SNI and
        the certificate's check are for the name, and the program sends and
        receives plain bytes. A handshake that fails is a refusal."""
        handle = self.handles.get(fd)
        if handle is None:
            return EBADF
        if handle["kind"] != SOCK_STREAM or handle["sock"] is not None:
            return EINVAL
        name = mem.read(host, host_len).decode()
        error = self._connect(handle, self.hosts.get(name, (name, port)))
        if error:
            return error
        try:
            handle["sock"] = self.tls.wrap_socket(handle["sock"], server_hostname=name)
        except (ssl.SSLError, ssl.CertificateError, OSError):
            handle["sock"].close()
            handle["sock"] = None
            return ECONNREFUSED
        return 0

    def _connect(self, handle, to):
        conn = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            conn.connect(to)
        except ConnectionRefusedError:
            conn.close()
            return ECONNREFUSED
        except TimeoutError:
            conn.close()
            return ETIMEDOUT
        except OSError:
            conn.close()
            return ENETUNREACH
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        handle["sock"] = conn
        return 0

    def send(self, mem, fd, buf, length, sent_out):
        handle = self.handles.get(fd)
        if handle is None or handle["sock"] is None:
            return EBADF
        try:
            handle["sock"].sendall(mem.read(buf, length))
        except OSError:
            return EIO
        mem.u32(sent_out, length)
        return 0

    def recv(self, mem, fd, buf, length, recv_out):
        handle = self.handles.get(fd)
        if handle is None or handle["sock"] is None:
            return EBADF
        try:
            data = pending(handle["sock"], length)
        except OSError:
            return EIO
        if not fits(len(data)):
            return EINVAL
        mem.write(buf, data)
        mem.u32(recv_out, len(data))
        return 0

    def sendto(self, mem, fd, buf, length, addr, addr_len, sent_out):
        handle = self.handles.get(fd)
        to = mem.sockaddr(addr, addr_len)
        if handle is None:
            return EBADF
        if handle["kind"] != SOCK_DGRAM or to is None:
            return EINVAL
        if to == MDNS and self.mdns_to:
            to = self.mdns_to
        if handle["sock"] is None:
            handle["sock"] = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            handle["sock"].sendto(mem.read(buf, length), to)
        except OSError:
            return EIO
        mem.u32(sent_out, length)
        return 0

    def close(self, mem, fd):
        handle = self.handles.pop(fd, None)
        if handle is None:
            return EBADF
        if handle["sock"] is not None:
            handle["sock"].close()
        return 0


class Terminal:
    """rootshell's terminal calls. Raw mode hands the program each key as
    it's typed, unechoed; where the input is a terminal, so does this."""

    def __init__(self):
        self.cooked = None

    def set_raw(self, mem, enabled):
        if os.isatty(0):
            import termios

            if enabled and self.cooked is None:
                self.cooked = termios.tcgetattr(0)
                raw = termios.tcgetattr(0)
                raw[3] &= ~(termios.ECHO | termios.ICANON | termios.ISIG)
                termios.tcsetattr(0, termios.TCSADRAIN, raw)
            elif not enabled and self.cooked is not None:
                termios.tcsetattr(0, termios.TCSADRAIN, self.cooked)
                self.cooked = None
        return 0

    def is_tty(self, mem, fd):
        # rootshell's answer, whatever the fd is attached to.
        return 1 if fd in (0, 1, 2) else 0


class Memory:
    """The program's memory, during one call."""

    def __init__(self, caller):
        self.caller = caller
        self.memory = caller.get("memory")

    def read(self, at, length):
        return bytes(self.memory.read(self.caller, at, at + length))

    def write(self, at, data):
        self.memory.write(self.caller, data, at)

    def i32(self, at, value):
        self.write(at, value.to_bytes(4, "little", signed=True))

    def u32(self, at, value):
        self.write(at, value.to_bytes(4, "little"))

    def sockaddr(self, at, length):
        """rootshell's sockaddr_in: family, a spare byte, port, address."""
        if length < 8:
            return None
        raw = self.read(at, 8)
        if raw[0] != AF_INET:
            return None
        return (".".join(str(b) for b in raw[4:8]), int.from_bytes(raw[2:4], "big"))


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--dir", default=".")
    parser.add_argument("--mdns")
    parser.add_argument("--device")
    parser.add_argument("--host", action="append", default=[])
    parser.add_argument("--ca")
    parser.add_argument("program")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    mdns_to = None
    if args.mdns:
        host, port = args.mdns.rsplit(":", 1)
        mdns_to = (host, int(port))
    hosts = {}
    for entry in args.host:
        name, to = entry.split("=", 1)
        host, port = to.rsplit(":", 1)
        hosts[name] = (host, int(port))
    sockets = Sockets(mdns_to, args.device, hosts, args.ca)
    terminal = Terminal()

    engine = wasmtime.Engine()
    module = wasmtime.Module.from_file(engine, args.program)
    linker = wasmtime.Linker(engine)
    linker.define_wasi()
    i32 = wasmtime.ValType.i32()
    calls = {
        ("rootshell_socket", sockets): {
            "socket": 3, "bind": 3, "listen": 2, "accept": 2, "connect": 3,
            "connect_host": 4, "tls_connect_host": 4,
            "send": 4, "recv": 4, "sendto": 6, "close": 1,
        },
        ("rootshell_terminal", terminal): {"set_raw": 1, "is_tty": 1},
    }
    for (module_name, answerer), names in calls.items():
        for name, count in names.items():
            def call(caller, *params, handler=getattr(answerer, name)):
                return handler(Memory(caller), *params)
            linker.define_func(
                module_name,
                f"{module_name}_{name}",
                wasmtime.FuncType([i32] * count, [i32]),
                call,
                access_caller=True,
            )

    store = wasmtime.Store(engine)
    wasi = wasmtime.WasiConfig()
    wasi.argv = ["./" + args.program.rsplit("/", 1)[-1], *args.arguments]
    # As rootshell sets them.
    wasi.env = [("HOME", "/"), ("PWD", "/"), ("USER", "mobile")]
    # rootshell's folder is the program's `/`.
    wasi.preopen_dir(args.dir, "/")
    wasi.inherit_stdin()
    wasi.inherit_stdout()
    wasi.inherit_stderr()
    store.set_wasi(wasi)
    # Files' reads come back through the same slot, and rootshell's broker
    # makes folders, renames and deletes with FileManager, which answers
    # differently from WASI's own calls. WASI's calls work on the memory of
    # the module that calls them, so a module of its own calls them for the
    # program, and what they bring only goes on to the program as rootshell
    # would hand it over.
    helper = linker.instantiate(store, wasmtime.Module(engine, HELPER))
    helper_calls = helper.exports(store)
    helper_memory = helper_calls["memory"]

    def put(caller, at, path):
        if len(path) > LONGEST_PATH:
            raise ValueError("rootshell.py: a path longer than it takes")
        helper_memory.write(caller, path, at)

    def kind(caller, fd, path):
        """What's at `path` in the folder `fd`: WASI's file type, or None
        when nothing's there."""
        put(caller, PATH_AT, path)
        if helper_calls["stat"](caller, fd, len(path)) != 0:
            return None
        return helper_memory.read(caller, STAT_AT + 16, STAT_AT + 17)[0]

    def on_path(name):
        """WASI's call `name` on one path, as rootshell's broker makes it,
        with FileManager: EIO when the path isn't there."""
        def call(caller, fd, path, length):
            path = Memory(caller).read(path, length)
            if kind(caller, fd, path) is None:
                return EIO
            put(caller, PATH_AT, path)
            return helper_calls[name](caller, fd, len(path))
        return call

    def path_create_directory(caller, fd, path, length):
        # FileManager's createDirectory, without the folders it's in: EIO
        # when the folder's there already, or the one it goes in isn't.
        path = Memory(caller).read(path, length)
        if kind(caller, fd, path) is not None or kind(caller, fd, parent(path)) is None:
            return EIO
        put(caller, PATH_AT, path)
        return helper_calls["mkdir"](caller, fd, len(path))

    def path_rename(caller, fd, path, length, to_fd, to_path, to_length):
        # FileManager's moveItem: EIO when what's moved isn't there, or
        # something's already where it goes, which POSIX would replace.
        memory = Memory(caller)
        path, to_path = memory.read(path, length), memory.read(to_path, to_length)
        if kind(caller, fd, path) is None or kind(caller, to_fd, to_path) is not None:
            return EIO
        put(caller, PATH_AT, path)
        put(caller, TO_AT, to_path)
        return helper_calls["rename"](caller, fd, len(path), to_fd, len(to_path))

    def fd_read(caller, fd, iovs, iovs_len, read_out):
        memory = Memory(caller)
        buffers = []
        for index in range(iovs_len):
            entry = memory.read(iovs + 8 * index, 8)
            buffers.append((int.from_bytes(entry[:4], "little"), int.from_bytes(entry[4:], "little")))
        wanted = sum(length for _, length in buffers)
        pages = (READ_AT + wanted + 0xFFFF) // 0x10000
        if helper_memory.size(caller) < pages:
            helper_memory.grow(caller, pages - helper_memory.size(caller))
        error = helper_calls["read"](caller, fd, wanted)
        if error != 0:
            return error
        read = int.from_bytes(helper_memory.read(caller, 8, 12), "little")
        if fd > 2 and not fits(read):
            return EINVAL
        data = bytes(helper_memory.read(caller, READ_AT, READ_AT + read))
        at = 0
        for buf, length in buffers:
            if at >= read:
                break
            memory.write(buf, data[at:at + length])
            at += length
        memory.u32(read_out, read)
        return 0

    linker.allow_shadowing = True
    for name, call, count in [
        ("fd_read", fd_read, 4),
        ("path_create_directory", path_create_directory, 3),
        ("path_unlink_file", on_path("unlink"), 3),
        ("path_remove_directory", on_path("rmdir"), 3),
        ("path_rename", path_rename, 6),
    ]:
        linker.define_func(
            "wasi_snapshot_preview1",
            name,
            wasmtime.FuncType([i32] * count, [i32]),
            call,
            access_caller=True,
        )
    try:
        instance = linker.instantiate(store, module)
    except wasmtime.WasmtimeError as error:
        # What rootshell's runtime says of a call it doesn't have.
        sys.exit(f"rootshell.py: the program can't start: {error}")
    try:
        instance.exports(store)["_start"](store)
    except wasmtime.ExitTrap as exit_trap:
        sys.exit(exit_trap.code)


if __name__ == "__main__":
    main()
