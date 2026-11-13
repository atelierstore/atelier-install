//! What rootshell lends the programs it runs, as this test uses it.
//!
//! WASI Preview 1 has no sockets, so rootshell has calls of its own
//! (`rootshell_socket_*`, in its `wasm/README.md`), which it answers with
//! Apple's Network.framework. Each blocks until it's done. There is no
//! poll and no timeout: a wait for a connection, or for what a connection
//! sends, lasts until one comes. A listener is on every address the iPhone
//! has, since rootshell keeps only the port of what a program binds, and a
//! program can't ask who connected.
//!
//! Off rootshell (`cargo run` on a computer, and `cargo test`) the same
//! calls go to the system's sockets, on this computer's own address only.

use std::fmt;

/// The most one receive asks for. rootshell hands the bytes back through
/// one 64 KB slot of shared memory, as base64 in JSON, and a receive that
/// brings more than fits fails, its bytes gone.
pub const MOST_RECEIVED: usize = 16 * 1024;

/// A call rootshell refused, with the error number it gave (WASI's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub call: &'static str,
    pub errno: i32,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed (error {})", self.call, self.errno)
    }
}

pub use imp::{Connection, Listener, check_in};

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{MOST_RECEIVED, Refused};

    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;

    /// The most one send carries: rootshell takes any length, as base64 in
    /// a message to the app, and smaller messages keep each one quick.
    const MOST_SENT: usize = 16 * 1024;

    #[link(wasm_import_module = "rootshell_socket")]
    unsafe extern "C" {
        fn rootshell_socket_socket(domain: i32, kind: i32, fd_out: *mut i32) -> i32;
        fn rootshell_socket_bind(fd: i32, addr: *const u8, addr_len: i32) -> i32;
        fn rootshell_socket_listen(fd: i32, backlog: i32) -> i32;
        fn rootshell_socket_accept(fd: i32, fd_out: *mut i32) -> i32;
        fn rootshell_socket_connect(fd: i32, addr: *const u8, addr_len: i32) -> i32;
        fn rootshell_socket_send(fd: i32, buf: *const u8, len: i32, sent_out: *mut u32) -> i32;
        fn rootshell_socket_recv(fd: i32, buf: *mut u8, len: i32, recv_out: *mut u32) -> i32;
        fn rootshell_socket_close(fd: i32) -> i32;
    }

    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    unsafe extern "C" {
        fn fd_prestat_get(fd: u32, prestat: *mut u32) -> u32;
    }

    fn check(call: &'static str, errno: i32) -> Result<(), Refused> {
        if errno == 0 {
            Ok(())
        } else {
            Err(Refused { call, errno })
        }
    }

    /// rootshell's `sockaddr_in`: the family in the first byte, the port
    /// big-endian, then the address.
    fn sockaddr(address: [u8; 4], port: u16) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = AF_INET as u8;
        out[2..4].copy_from_slice(&port.to_be_bytes());
        out[4..8].copy_from_slice(&address);
        out
    }

    fn socket() -> Result<i32, Refused> {
        let mut fd = -1;
        // SAFETY: rootshell writes the new socket's number there.
        check("socket", unsafe {
            rootshell_socket_socket(AF_INET, SOCK_STREAM, &mut fd)
        })?;
        Ok(fd)
    }

    /// A port this program listens on, closed when it's dropped.
    pub struct Listener {
        fd: i32,
    }

    impl Listener {
        /// Listens on `port`. rootshell answers before its listener is
        /// ready, and says nothing if the port can't be had.
        pub fn on(port: u16) -> Result<Self, Refused> {
            let listener = Listener { fd: socket()? };
            let addr = sockaddr([0, 0, 0, 0], port);
            // SAFETY: `addr` is the 16 bytes its length says.
            check("bind", unsafe {
                rootshell_socket_bind(listener.fd, addr.as_ptr(), addr.len() as i32)
            })?;
            // SAFETY: only numbers go over.
            check("listen", unsafe {
                rootshell_socket_listen(listener.fd, 16)
            })?;
            Ok(listener)
        }

        /// Waits for the next connection, however long it takes.
        pub fn accept(&self) -> Result<Connection, Refused> {
            let mut fd = -1;
            // SAFETY: rootshell writes the connection's number there.
            check("accept", unsafe {
                rootshell_socket_accept(self.fd, &mut fd)
            })?;
            Ok(Connection { fd })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            // SAFETY: only a number goes over.
            unsafe { rootshell_socket_close(self.fd) };
        }
    }

    /// A TCP connection, closed when it's dropped.
    pub struct Connection {
        fd: i32,
    }

    impl Connection {
        /// Connects to `port` on this iPhone itself.
        pub fn to_this_iphone(port: u16) -> Result<Self, Refused> {
            let connection = Connection { fd: socket()? };
            let addr = sockaddr([127, 0, 0, 1], port);
            // SAFETY: `addr` is the 16 bytes its length says.
            check("connect", unsafe {
                rootshell_socket_connect(connection.fd, addr.as_ptr(), addr.len() as i32)
            })?;
            Ok(connection)
        }

        /// Waits for what the other side sends, however long it takes, and
        /// puts it in `buf`. 0 is the other side closing.
        pub fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Refused> {
            if buf.is_empty() {
                return Ok(0);
            }
            let asked = buf.len().min(MOST_RECEIVED);
            let mut got = 0u32;
            // SAFETY: rootshell writes `asked` bytes at most into `buf`.
            check("recv", unsafe {
                rootshell_socket_recv(self.fd, buf.as_mut_ptr(), asked as i32, &mut got)
            })?;
            Ok((got as usize).min(asked))
        }

        /// Sends all of `bytes`.
        pub fn send(&mut self, bytes: &[u8]) -> Result<(), Refused> {
            for piece in bytes.chunks(MOST_SENT) {
                let mut sent = 0u32;
                // SAFETY: `piece` is the bytes its length says.
                check("send", unsafe {
                    rootshell_socket_send(self.fd, piece.as_ptr(), piece.len() as i32, &mut sent)
                })?;
                if sent as usize != piece.len() {
                    return Err(Refused {
                        call: "send",
                        errno: 29,
                    });
                }
            }
            Ok(())
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            // SAFETY: only a number goes over.
            unsafe { rootshell_socket_close(self.fd) };
        }
    }

    /// Asks rootshell which folder it opened for the program: a call that
    /// goes to the app and back and changes nothing. Atelier's installer
    /// makes it every 50 ms of a long computation, which rootshell's web
    /// view otherwise stops (its src/awake.rs).
    pub fn check_in() {
        let mut prestat = [0u32; 2];
        // SAFETY: what WASI writes there, if anything, is a prestat's 8 bytes.
        unsafe { fd_prestat_get(3, prestat.as_mut_ptr()) };
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{MOST_RECEIVED, Refused};
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    fn refused(call: &'static str) -> impl Fn(std::io::Error) -> Refused {
        move |error| Refused {
            call,
            errno: error.raw_os_error().unwrap_or(29),
        }
    }

    pub struct Listener(TcpListener);

    impl Listener {
        pub fn on(port: u16) -> Result<Self, Refused> {
            TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                .map(Listener)
                .map_err(refused("listen"))
        }

        pub fn accept(&self) -> Result<Connection, Refused> {
            let (stream, _) = self.0.accept().map_err(refused("accept"))?;
            Ok(Connection(stream))
        }
    }

    pub struct Connection(TcpStream);

    impl Connection {
        pub fn to_this_iphone(port: u16) -> Result<Self, Refused> {
            TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .map(Connection)
                .map_err(refused("connect"))
        }

        pub fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Refused> {
            let asked = buf.len().min(MOST_RECEIVED);
            self.0.read(&mut buf[..asked]).map_err(refused("recv"))
        }

        pub fn send(&mut self, bytes: &[u8]) -> Result<(), Refused> {
            self.0.write_all(bytes).map_err(refused("send"))
        }
    }

    /// Off rootshell, nothing stops the program.
    pub fn check_in() {}
}
