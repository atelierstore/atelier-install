//! A port the program listens on, asked for a connection without waiting
//! for one: the page's (`page.rs`), and the pairing's while the page is up.
//!
//! rootshell's accept waits until a connection comes, and nothing looks
//! first or wakes it (`net.rs`). So before it accepts, the program connects
//! to its own port and sends a word nobody else knows, a knock: accept then
//! returns at once, with another connection if one came first, since
//! rootshell hands them over in the order they came, or with the program's
//! own, which says nobody was waiting. Tried on the iPhone on 13 Nov 2026
//! (the page test, on the branch `rootshell-page-test`): a knock took 7 to
//! 16 ms.
//!
//! It knocks a few times before its first accept, and numbers its knocks.
//! rootshell keeps an accept that a program was stopped in (Control-C),
//! under the socket's number, which the next program's listener gets too,
//! and hands that accept the listener's first connection, which is lost: a
//! knock that comes back says the ones before it won't.
//!
//! A connection that sends nothing holds the program up until it closes,
//! as there's no time limit on a receive. Safari opened none in that test.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::net::{Listener, Stream};

/// How long the word is, and a knock: the word, then the knock's number.
const WORD: usize = 16;
const KNOCK: usize = WORD + 4;
/// How many times the program knocks before its first accept.
const FIRST_KNOCKS: u32 = 4;
/// Room for the knocks and the connections that come meanwhile.
const QUEUE: i32 = 16;
/// The most of a connection's first bytes read to tell a knock.
const FIRST_READ: usize = 4096;

pub struct Door {
    listener: Listener,
    port: u16,
    word: [u8; WORD],
    /// How many times the program has knocked.
    knocked: u32,
    /// Its own connections that accept hasn't handed back, oldest first,
    /// each with its knock's number.
    ours: VecDeque<(u32, Stream)>,
    /// A connection that came before the first knock came back.
    early: Option<Visitor>,
}

/// A connection that came, which reads what it sent first again: that was
/// read to tell it from a knock.
#[derive(Debug)]
pub struct Visitor {
    stream: Stream,
    first: Vec<u8>,
    at: usize,
}

impl Read for Visitor {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.at < self.first.len() {
            let count = buf.len().min(self.first.len() - self.at);
            buf[..count].copy_from_slice(&self.first[self.at..self.at + count]);
            self.at += count;
            return Ok(count);
        }
        self.stream.read(buf)
    }
}

impl Write for Visitor {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Door {
    /// Listens on `port`, and returns once a knock has come back: the port
    /// is this program's. It's another listener's when a knock can't be
    /// sent, since that one drops what connects: one that rootshell never
    /// closed, or another tab's.
    pub fn open(port: u16) -> io::Result<Self> {
        let listener = Listener::bind_queue(port, QUEUE)?;
        // rootshell answers `listen` before its listener is ready.
        pause(300);
        let mut door = Door {
            listener,
            port,
            word: random_word(),
            knocked: 0,
            ours: VecDeque::new(),
            early: None,
        };
        let mut connected = door.connect();
        for _ in 0..10 {
            if connected.is_ok() {
                break;
            }
            pause(100);
            connected = door.connect();
        }
        connected?;
        for _ in 1..FIRST_KNOCKS {
            door.connect()?;
        }
        let taken = |_| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("port {port} is another program's"),
            )
        };
        let word = door.word;
        for (number, ours) in &mut door.ours {
            ours.write_all(&knock(&word, *number)).map_err(taken)?;
        }
        // A connection that was dropped takes what's sent once, then not.
        for _ in 0..4 {
            pause(80);
            for (_, ours) in &mut door.ours {
                ours.write_all(b"\n").map_err(taken)?;
            }
        }
        door.early = door.accept(false)?;
        Ok(door)
    }

    /// The next connection, if one is waiting, without waiting for one.
    pub fn next(&mut self) -> io::Result<Option<Visitor>> {
        if let Some(early) = self.early.take() {
            return Ok(Some(early));
        }
        if self.ours.is_empty() {
            self.connect()?;
            let number = self.knocked;
            if let Some((_, ours)) = self.ours.back_mut() {
                ours.write_all(&knock(&self.word, number))?;
            }
        }
        self.accept(false)
    }

    /// The next connection, however long it takes to come.
    pub fn wait(&mut self) -> io::Result<Visitor> {
        if let Some(early) = self.early.take() {
            return Ok(early);
        }
        loop {
            if let Some(visitor) = self.accept(true)? {
                return Ok(visitor);
            }
        }
    }

    /// Connects to the program's own port, for a knock.
    fn connect(&mut self) -> io::Result<()> {
        let ours = Stream::connect(Ipv4Addr::LOCALHOST, self.port)?;
        self.knocked += 1;
        self.ours.push_back((self.knocked, ours));
        Ok(())
    }

    /// Accepts until another connection comes, or, unless `waiting`, the
    /// last of the program's own does.
    fn accept(&mut self, waiting: bool) -> io::Result<Option<Visitor>> {
        loop {
            let mut stream = self.listener.accept()?;
            let mut first = vec![0u8; FIRST_READ];
            // A connection that breaks is one that closed.
            let mut got = stream.read(&mut first).unwrap_or(0);
            // A knock, if it's that, may come in pieces.
            while got > 0 && got < KNOCK && first[..got.min(WORD)] == self.word[..got.min(WORD)] {
                match stream.read(&mut first[got..]) {
                    Ok(more) if more > 0 => got += more,
                    _ => break,
                }
            }
            if got >= KNOCK && first[..WORD] == self.word {
                let mut number = [0u8; 4];
                number.copy_from_slice(&first[WORD..KNOCK]);
                let number = u32::from_be_bytes(number);
                // It's back, and the ones before it never will be.
                while self.ours.front().is_some_and(|(ours, _)| *ours <= number) {
                    self.ours.pop_front();
                }
                if self.ours.is_empty() && !waiting {
                    return Ok(None);
                }
                continue;
            }
            first.truncate(got);
            return Ok(Some(Visitor {
                stream,
                first,
                at: 0,
            }));
        }
    }
}

fn knock(word: &[u8; WORD], number: u32) -> [u8; KNOCK] {
    let mut knock = [0u8; KNOCK];
    knock[..WORD].copy_from_slice(word);
    knock[WORD..].copy_from_slice(&number.to_be_bytes());
    knock
}

fn random_word() -> [u8; WORD] {
    let mut word = [0u8; WORD];
    getrandom::getrandom(&mut word).expect("the system's random source");
    word
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A door on a port nothing else listens on. Off rootshell and under
    /// the harness alike, so with the program's own sockets, and one
    /// thread: what a connection sends waits for the program in the
    /// system's buffers.
    pub(crate) fn opened() -> (Door, u16) {
        for _ in 0..20 {
            let mut word = [0u8; 2];
            getrandom::getrandom(&mut word).unwrap();
            let port = 30000 + u16::from_le_bytes(word) % 20000;
            if let Ok(door) = Door::open(port) {
                return (door, port);
            }
        }
        panic!("no free port");
    }

    pub(crate) fn browser(port: u16, request: &[u8]) -> Stream {
        let mut stream = Stream::connect(Ipv4Addr::LOCALHOST, port).unwrap();
        stream.write_all(request).unwrap();
        stream
    }

    pub(crate) fn read_all(mut stream: Stream) -> Vec<u8> {
        let mut read = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => return read,
                Ok(got) => read.extend_from_slice(&buf[..got]),
            }
        }
    }

    #[test]
    fn says_nobody_came_without_waiting() {
        let (mut door, _) = opened();
        for _ in 0..3 {
            assert!(door.next().unwrap().is_none());
        }
    }

    #[test]
    fn hands_over_a_connection_with_what_it_sent_first() {
        let (mut door, port) = opened();
        let browser = browser(port, b"GET / HTTP/1.1\r\n\r\n");
        let mut visitor = door.next().unwrap().expect("the browser's connection");
        let mut read = [0u8; 18];
        visitor.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"GET / HTTP/1.1\r\n\r\n");
        visitor.write_all(b"ok").unwrap();
        drop(visitor);
        assert_eq!(read_all(browser), b"ok");
        // Its own knock, which came after.
        assert!(door.next().unwrap().is_none());
        assert!(door.ours.is_empty());
    }

    #[test]
    fn waits_past_its_own_knocks() {
        let (mut door, port) = opened();
        assert!(door.next().unwrap().is_none());
        // A knock that's still out when the program waits, then a browser.
        door.connect().unwrap();
        let (word, number) = (door.word, door.knocked);
        door.ours
            .back_mut()
            .unwrap()
            .1
            .write_all(&knock(&word, number))
            .unwrap();
        let browser = browser(port, b"hello");
        let mut visitor = door.wait().unwrap();
        let mut read = [0u8; 5];
        visitor.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hello");
        assert!(door.ours.is_empty());
        drop(browser);
    }

    #[test]
    fn a_port_already_taken_isnt_opened() {
        let (_door, port) = opened();
        assert!(Door::open(port).is_err());
    }
}
