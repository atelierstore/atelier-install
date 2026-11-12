//! The tunnel's TCP stack without a task to run it. rootshell's programs
//! have one thread and blocking sockets, so each step on a connection drives
//! the stack itself until it's done, through [`run`]: connecting waits for
//! the iPhone's answer, a read for the next bytes, a write until all of it
//! is on its way.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

use idevice::provider::RsdProvider;
use idevice::tcp::adapter::Adapter;
use idevice::{IdeviceError, ReadWrite};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::blocking::{NotReady, run};

/// The tunnel's TCP stack, shared by the connections through it.
#[derive(Debug, Clone)]
pub struct Tunnel(Arc<Mutex<Adapter>>);

impl Tunnel {
    pub fn new(adapter: Adapter) -> Self {
        Tunnel(Arc::new(Mutex::new(adapter)))
    }

    /// A connection to `port` on the iPhone's side of the tunnel.
    pub fn connect(&self, port: u16) -> io::Result<Connection> {
        let local = finish(run(self.stack().connect(port)))?;
        Ok(Connection {
            tunnel: self.clone(),
            local,
            unread: Vec::new(),
            closed: false,
        })
    }

    fn stack(&self) -> MutexGuard<'_, Adapter> {
        // Nothing unwinds (panics abort), so a poisoned lock can't happen.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl RsdProvider for Tunnel {
    fn connect_to_service_port(
        &mut self,
        port: u16,
    ) -> impl Future<Output = Result<Box<dyn ReadWrite>, IdeviceError>> + Send {
        std::future::ready(
            self.connect(port)
                .map(|connection| Box::new(connection) as Box<dyn ReadWrite>)
                .map_err(IdeviceError::Socket),
        )
    }
}

/// One TCP connection through the tunnel.
#[derive(Debug)]
pub struct Connection {
    tunnel: Tunnel,
    /// Its port on this side, which the stack knows it by.
    local: u16,
    /// What the stack handed over beyond what the last read had room for.
    unread: Vec<u8>,
    closed: bool,
}

impl Connection {
    fn close(&mut self) -> io::Result<()> {
        if std::mem::replace(&mut self.closed, true) {
            return Ok(());
        }
        finish(run(self.tunnel.stack().close(self.local)))
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.unread.is_empty() {
            match finish(run(this.tunnel.stack().recv(this.local))) {
                Ok(bytes) => this.unread = bytes,
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
        let n = this.unread.len().min(buf.remaining());
        buf.put_slice(&this.unread[..n]);
        this.unread.drain(..n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let sent = finish(run(self.tunnel.stack().send_all(self.local, buf)));
        Poll::Ready(sent.map(|()| buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // A write returns once all of it is on its way.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.close())
    }
}

/// A step's own result, or an error for one that waited for something other
/// than the tunnel, such as a timer.
fn finish<T>(result: Result<io::Result<T>, NotReady>) -> io::Result<T> {
    result.unwrap_or_else(|NotReady| {
        Err(io::Error::other(
            "the tunnel's TCP stack waited for something other than the tunnel",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::net::{IpAddr, Ipv6Addr};

    use idevice::tcp::packets::{Ipv6Packet, ProtocolNumber, TcpFlags, TcpPacket};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const HOST: Ipv6Addr = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
    const PHONE: Ipv6Addr = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 2);
    const ECHO: u16 = 7;

    /// A stand-in iPhone at the tunnel's far end. It takes connections to
    /// [`ECHO`] only, acknowledges each segment at once and sends what it
    /// got back in capitals. With nothing to send, a read waits, as a
    /// blocking socket would.
    #[derive(Debug, Default)]
    struct EchoPhone {
        to_host: VecDeque<u8>,
        /// Each connection's next sequence number, by the host's port.
        next: HashMap<u16, u32>,
    }

    impl EchoPhone {
        /// One IP packet from the host: the stack writes whole packets.
        fn answer(&mut self, packet: &[u8]) {
            // IPv6's fixed header: the payload's length at 4, then the
            // protocol, and the payload from 40.
            assert_eq!(packet[0] >> 4, 6, "the host sent something other than IPv6");
            assert_eq!(packet[6], ProtocolNumber::Tcp as u8);
            let length = u16::from_be_bytes([packet[4], packet[5]]) as usize;
            assert_eq!(packet.len(), 40 + length, "the host wrote part of a packet");
            let tcp = TcpPacket::parse(&packet[40..]).unwrap();
            let port = tcp.source_port;
            if tcp.destination_port != ECHO {
                return;
            }
            if tcp.flags.syn {
                let flags = TcpFlags {
                    syn: true,
                    ack: true,
                    ..Default::default()
                };
                self.send(port, 1000, tcp.sequence_number.wrapping_add(1), flags, &[]);
                self.next.insert(port, 1001);
                return;
            }
            if tcp.payload.is_empty() && !tcp.flags.fin {
                return;
            }
            let ack = tcp
                .sequence_number
                .wrapping_add(tcp.payload.len() as u32)
                .wrapping_add(tcp.flags.fin as u32);
            let echo = tcp.payload.to_ascii_uppercase();
            let Some(seq) = self.next.get_mut(&port) else {
                return;
            };
            let at = *seq;
            *seq = at.wrapping_add(echo.len() as u32);
            let flags = TcpFlags {
                ack: true,
                psh: !echo.is_empty(),
                ..Default::default()
            };
            self.send(port, at, ack, flags, &echo);
        }

        fn send(&mut self, port: u16, seq: u32, ack: u32, flags: TcpFlags, payload: &[u8]) {
            let tcp = TcpPacket::create(
                IpAddr::V6(PHONE),
                IpAddr::V6(HOST),
                ECHO,
                port,
                seq,
                ack,
                flags,
                u16::MAX - 1,
                &[],
                payload,
            );
            self.to_host.extend(Ipv6Packet::create(
                PHONE,
                HOST,
                ProtocolNumber::Tcp,
                64,
                &tcp,
            ));
        }
    }

    impl AsyncRead for EchoPhone {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.to_host.is_empty() {
                return Poll::Pending;
            }
            let n = self.to_host.len().min(buf.remaining());
            let bytes: Vec<u8> = self.to_host.drain(..n).collect();
            buf.put_slice(&bytes);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for EchoPhone {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.answer(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn tunnel() -> Tunnel {
        let phone = Box::new(EchoPhone::default());
        Tunnel::new(Adapter::new(phone, IpAddr::V6(HOST), IpAddr::V6(PHONE)))
    }

    /// Runs `body` where the stack can run: on WASI as it is, elsewhere in
    /// a tokio runtime, for the timer jktcp's reads race against there.
    fn on_stack(body: impl FnOnce()) {
        #[cfg(not(target_os = "wasi"))]
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        #[cfg(not(target_os = "wasi"))]
        let _inside = runtime.enter();
        body();
    }

    fn read(connection: &mut Connection, length: usize, piece: usize) -> Vec<u8> {
        let mut got = vec![0u8; length];
        for chunk in got.chunks_mut(piece) {
            run(connection.read_exact(chunk)).unwrap().unwrap();
        }
        got
    }

    #[test]
    fn a_connection_sends_more_than_the_window_and_reads_it_back_in_pieces() {
        on_stack(|| {
            let mut connection = tunnel().connect(ECHO).unwrap();
            // 64 KB is all the phone takes before it acknowledges, so the
            // write has to read its acknowledgments as it goes.
            let sent: Vec<u8> = (0..200_000u32).map(|i| b'a' + (i % 26) as u8).collect();
            run(connection.write_all(&sent)).unwrap().unwrap();
            let got = read(&mut connection, sent.len(), 1000);
            assert_eq!(got, sent.to_ascii_uppercase());
        });
    }

    #[test]
    fn connections_through_one_tunnel_get_their_own_bytes() {
        on_stack(|| {
            let mut tunnel = tunnel();
            let mut first = tunnel.connect(ECHO).unwrap();
            let mut second = run(tunnel.connect_to_service_port(ECHO)).unwrap().unwrap();
            run(first.write_all(b"first")).unwrap().unwrap();
            run(second.write_all(b"second")).unwrap().unwrap();
            let mut got = [0u8; 6];
            run(second.read_exact(&mut got)).unwrap().unwrap();
            assert_eq!(&got, b"SECOND");
            assert_eq!(read(&mut first, 5, 5), b"FIRST");
            drop(second);
            run(first.write_all(b"again")).unwrap().unwrap();
            assert_eq!(read(&mut first, 5, 2), b"AGAIN");
            run(first.shutdown()).unwrap().unwrap();
        });
    }

    #[test]
    fn a_step_that_waits_for_nothing_the_tunnel_brings_fails() {
        on_stack(|| {
            // The phone never answers on this port, and a read with nothing
            // to read waits: on rootshell, for good.
            let error = tunnel().connect(9).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("something other than the tunnel")
            );
        });
    }
}
