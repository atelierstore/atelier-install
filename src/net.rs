//! The network, as rootshell lends it to the programs it runs.
//!
//! WASI Preview 1 has no sockets, so rootshell has calls of its own
//! (`rootshell_socket_*`, in its `wasm/README.md`), which it answers with
//! Apple's Network.framework. Each blocks until it's done, and there is no
//! poll: one thing happens at a time. Of what its documentation lists, its
//! runtime has no `getsockname`, so a listener's port is one this program
//! picks, and no way to tell which addresses the iPhone has.
//!
//! rootshell hands what a connection received back through one 64 KB slot
//! of shared memory, as base64 in JSON, and a receive that brings more than
//! fits fails with EINVAL, its bytes gone (its Resources/Wasm/wasm-host.js).
//! So a receive here asks for [`MOST_RECEIVED`] at most, and the program's
//! readers take the short read, as from any socket.
//!
//! Off rootshell (the tests, and trying the program on a computer) the same
//! calls go to the system's sockets, but for TLS, which is rootshell's.

use std::io::{self, Read, Write};
use std::net::Ipv4Addr;

pub use imp::Stream;
pub use imp::{Listener, send_datagram};

/// The most one receive asks rootshell for: its base64, 43,692 bytes, and
/// the JSON around it fit the 64 KB slot with room to spare.
#[allow(dead_code, reason = "rootshell's, and the tests'")]
pub const MOST_RECEIVED: usize = 32 * 1024;

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    const AF_INET: i32 = 2;
    const SOCK_STREAM: i32 = 1;
    const SOCK_DGRAM: i32 = 2;

    #[link(wasm_import_module = "rootshell_socket")]
    unsafe extern "C" {
        fn rootshell_socket_socket(domain: i32, kind: i32, fd_out: *mut i32) -> i32;
        fn rootshell_socket_bind(fd: i32, addr: *const u8, addr_len: i32) -> i32;
        fn rootshell_socket_listen(fd: i32, backlog: i32) -> i32;
        fn rootshell_socket_accept(fd: i32, fd_out: *mut i32) -> i32;
        fn rootshell_socket_connect(fd: i32, addr: *const u8, addr_len: i32) -> i32;
        fn rootshell_socket_connect_host(fd: i32, host: *const u8, host_len: i32, port: i32)
        -> i32;
        fn rootshell_socket_tls_connect_host(
            fd: i32,
            host: *const u8,
            host_len: i32,
            port: i32,
        ) -> i32;
        fn rootshell_socket_send(fd: i32, buf: *const u8, len: i32, sent_out: *mut u32) -> i32;
        fn rootshell_socket_recv(fd: i32, buf: *mut u8, len: i32, recv_out: *mut u32) -> i32;
        fn rootshell_socket_sendto(
            fd: i32,
            buf: *const u8,
            len: i32,
            addr: *const u8,
            addr_len: i32,
            sent_out: *mut u32,
        ) -> i32;
        fn rootshell_socket_close(fd: i32) -> i32;
    }

    /// rootshell's `sockaddr_in`: the family in the first byte, the port
    /// big-endian, then the address.
    fn sockaddr(address: Ipv4Addr, port: u16) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0] = AF_INET as u8;
        out[2..4].copy_from_slice(&port.to_be_bytes());
        out[4..8].copy_from_slice(&address.octets());
        out
    }

    /// rootshell answers with WASI's error numbers.
    fn check(what: &str, errno: i32) -> io::Result<()> {
        if errno == 0 {
            return Ok(());
        }
        use io::ErrorKind::*;
        let (kind, name) = match errno {
            1 => (AddrInUse, "the port is in use"),
            2 => (PermissionDenied, "not allowed"),
            8 => (NotConnected, "the socket is closed"),
            // Also what its TLS says of a certificate it doesn't trust.
            14 => (ConnectionRefused, "connection refused"),
            27 => (Interrupted, "interrupted"),
            28 => (InvalidInput, "invalid argument"),
            29 => (BrokenPipe, "the connection broke"),
            44 => (NotFound, "not found"),
            50 => (NetworkUnreachable, "the network is unreachable"),
            52 => (Unsupported, "rootshell has no such call"),
            73 => (TimedOut, "timed out"),
            _ => (Other, "error"),
        };
        Err(io::Error::new(kind, format!("{what}: {name} ({errno})")))
    }

    fn socket(kind: i32) -> io::Result<i32> {
        let mut fd = -1;
        check("socket", unsafe {
            rootshell_socket_socket(AF_INET, kind, &mut fd)
        })?;
        Ok(fd)
    }

    #[derive(Debug)]
    pub struct Listener {
        fd: i32,
    }

    impl Listener {
        /// Listens on `port`, on every address the iPhone has.
        pub fn bind(port: u16) -> io::Result<Self> {
            let fd = socket(SOCK_STREAM)?;
            let listener = Listener { fd };
            let addr = sockaddr(Ipv4Addr::UNSPECIFIED, port);
            check("bind", unsafe {
                rootshell_socket_bind(fd, addr.as_ptr(), addr.len() as i32)
            })?;
            check("listen", unsafe { rootshell_socket_listen(fd, 4) })?;
            Ok(listener)
        }

        /// Waits for the next connection.
        pub fn accept(&self) -> io::Result<Stream> {
            let mut fd = -1;
            check("accept", unsafe {
                rootshell_socket_accept(self.fd, &mut fd)
            })?;
            Ok(Stream { fd })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe { rootshell_socket_close(self.fd) };
        }
    }

    #[derive(Debug)]
    pub struct Stream {
        fd: i32,
    }

    impl Stream {
        /// Connects to `port` at `address`.
        pub fn connect(address: Ipv4Addr, port: u16) -> io::Result<Self> {
            let stream = Stream {
                fd: socket(SOCK_STREAM)?,
            };
            let addr = sockaddr(address, port);
            check("connect", unsafe {
                rootshell_socket_connect(stream.fd, addr.as_ptr(), addr.len() as i32)
            })?;
            Ok(stream)
        }

        /// Connects to `host`, a name or an address, at `port`, through
        /// TLS when `tls` is true. rootshell resolves the name, and its TLS
        /// is Network.framework's: it checks the server's certificate
        /// against Apple's trust store, and `send` and `recv` take and give
        /// the plaintext.
        pub fn connect_host(host: &str, port: u16, tls: bool) -> io::Result<Self> {
            let stream = Stream {
                fd: socket(SOCK_STREAM)?,
            };
            let (what, errno) = if tls {
                ("TLS connect", unsafe {
                    rootshell_socket_tls_connect_host(
                        stream.fd,
                        host.as_ptr(),
                        host.len() as i32,
                        i32::from(port),
                    )
                })
            } else {
                ("connect", unsafe {
                    rootshell_socket_connect_host(
                        stream.fd,
                        host.as_ptr(),
                        host.len() as i32,
                        i32::from(port),
                    )
                })
            };
            check(what, errno)?;
            Ok(stream)
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let mut got = 0u32;
            let asked = buf.len().min(MOST_RECEIVED);
            check("recv", unsafe {
                rootshell_socket_recv(self.fd, buf.as_mut_ptr(), asked as i32, &mut got)
            })?;
            Ok(got as usize)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let mut sent = 0u32;
            check("send", unsafe {
                rootshell_socket_send(self.fd, buf.as_ptr(), buf.len() as i32, &mut sent)
            })?;
            Ok(sent as usize)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            unsafe { rootshell_socket_close(self.fd) };
        }
    }

    /// Sends one UDP datagram, from a socket of its own.
    pub fn send_datagram(to: Ipv4Addr, port: u16, bytes: &[u8]) -> io::Result<()> {
        let fd = socket(SOCK_DGRAM)?;
        let addr = sockaddr(to, port);
        let mut sent = 0u32;
        let result = check("sendto", unsafe {
            rootshell_socket_sendto(
                fd,
                bytes.as_ptr(),
                bytes.len() as i32,
                addr.as_ptr(),
                addr.len() as i32,
                &mut sent,
            )
        });
        unsafe { rootshell_socket_close(fd) };
        result?;
        if sent as usize != bytes.len() {
            return Err(io::Error::other(format!(
                "sendto: sent {sent} of {} bytes",
                bytes.len()
            )));
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::*;
    use std::net::{TcpListener, TcpStream, UdpSocket};

    #[derive(Debug)]
    pub struct Listener(TcpListener);

    impl Listener {
        pub fn bind(port: u16) -> io::Result<Self> {
            TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).map(Listener)
        }

        pub fn accept(&self) -> io::Result<Stream> {
            let (stream, _) = self.0.accept()?;
            let _ = stream.set_nodelay(true);
            Ok(Stream(stream))
        }
    }

    #[derive(Debug)]
    pub struct Stream(TcpStream);

    impl Stream {
        pub fn connect(address: Ipv4Addr, port: u16) -> io::Result<Self> {
            let stream = TcpStream::connect((address, port))?;
            let _ = stream.set_nodelay(true);
            Ok(Stream(stream))
        }

        pub fn connect_host(host: &str, port: u16, tls: bool) -> io::Result<Self> {
            if tls {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "TLS is rootshell's, and this isn't rootshell",
                ));
            }
            let stream = TcpStream::connect((host, port))?;
            let _ = stream.set_nodelay(true);
            Ok(Stream(stream))
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    pub fn send_datagram(to: Ipv4Addr, port: u16, bytes: &[u8]) -> io::Result<()> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        socket.send_to(bytes, (to, port))?;
        Ok(())
    }
}
