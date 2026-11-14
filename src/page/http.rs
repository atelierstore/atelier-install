//! What a browser asks the page over HTTP, and what the program answers,
//! as the page test read and wrote them on the iPhone (13 Nov 2026).

use std::io::Read;

/// The longest request head the program reads, and the longest body: an
/// answer holds an Apple ID and a password at most.
const LONGEST_HEAD: usize = 16 * 1024;
const LONGEST_BODY: usize = 4 * 1024;

/// One request: `GET /state` and its headers, or a POST with its body.
#[derive(PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, without what follows a `?`.
    pub path: String,
    /// The headers, their names in lowercase.
    pub headers: Vec<(String, String)>,
    /// Never in the log: it can hold a password.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.method, self.path)
    }
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Why a connection brought no request.
#[derive(Debug, PartialEq, Eq)]
pub enum Nothing {
    /// It closed without sending anything.
    Silent,
    /// It stopped before its request was whole.
    CutShort,
    /// What it sent isn't HTTP: a browser trying HTTPS, for one.
    NotHttp,
    /// Its request is longer than the program reads.
    TooLong,
}

/// Reads one request from `connection`.
pub fn read(connection: &mut impl Read) -> Result<Request, Nothing> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    // A connection that breaks is one that closed.
    let mut more = |buf: &mut [u8]| connection.read(buf).unwrap_or(0);
    let head_ends = loop {
        if let Some(at) = find(&bytes, b"\r\n\r\n") {
            break at;
        }
        // A request starts with its method, in capital letters.
        if bytes
            .iter()
            .take(8)
            .any(|byte| !byte.is_ascii_graphic() && *byte != b' ')
        {
            return Err(Nothing::NotHttp);
        }
        if bytes.len() > LONGEST_HEAD {
            return Err(Nothing::TooLong);
        }
        let got = more(&mut buf);
        if got == 0 {
            return Err(if bytes.is_empty() {
                Nothing::Silent
            } else {
                Nothing::CutShort
            });
        }
        bytes.extend_from_slice(&buf[..got]);
    };

    let head = std::str::from_utf8(&bytes[..head_ends]).map_err(|_| Nothing::NotHttp)?;
    let mut lines = head.split("\r\n");
    let mut first_line = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) = (
        first_line.next(),
        first_line.next(),
        first_line.next(),
        first_line.next(),
    ) else {
        return Err(Nothing::NotHttp);
    };
    if !version.starts_with("HTTP/1.")
        || method.is_empty()
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
        || !target.starts_with('/')
    {
        return Err(Nothing::NotHttp);
    }
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();

    let mut request = Request {
        method: method.to_string(),
        path: target.split('?').next().unwrap_or_default().to_string(),
        headers,
        body: bytes.split_off(head_ends + 4),
    };
    let length = match request.header("content-length") {
        None => 0,
        Some(length) => length.parse::<usize>().map_err(|_| Nothing::NotHttp)?,
    };
    if length > LONGEST_BODY {
        return Err(Nothing::TooLong);
    }
    while request.body.len() < length {
        let got = more(&mut buf);
        if got == 0 {
            return Err(Nothing::CutShort);
        }
        request.body.extend_from_slice(&buf[..got]);
    }
    request.body.truncate(length);
    Ok(request)
}

fn find(bytes: &[u8], wanted: &[u8]) -> Option<usize> {
    bytes
        .windows(wanted.len())
        .position(|window| window == wanted)
}

/// What the page may load and ask: its own files and the program, nothing
/// from elsewhere, nothing inline, and no frame around it.
const POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
     font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; \
     frame-ancestors 'none'";

/// A whole answer: its head, which tells the browser to close the
/// connection and keep nothing, and its body.
pub fn answer(status: &str, kind: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {kind}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: {POLICY}\r\n\
         \r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection that brings `pieces`, one per read, then closes.
    struct Pieces<'a>(std::slice::Iter<'a, &'a [u8]>);

    impl Read for Pieces<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.next() {
                Some(piece) => {
                    buf[..piece.len()].copy_from_slice(piece);
                    Ok(piece.len())
                }
                None => Ok(0),
            }
        }
    }

    fn read_pieces(pieces: &[&[u8]]) -> Result<Request, Nothing> {
        read(&mut Pieces(pieces.iter()))
    }

    #[test]
    fn reads_a_request_and_its_headers() {
        let request = read_pieces(&[
            b"GET /state?x=1 HTTP/1.1\r\nHost: 127.0.0.1:24680\r\nX-Atelier-Key:  abc \r\n\r\n",
        ])
        .unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/state");
        assert_eq!(request.header("host"), Some("127.0.0.1:24680"));
        assert_eq!(request.header("x-atelier-key"), Some("abc"));
        assert_eq!(request.header("origin"), None);
        assert!(request.body.is_empty());
    }

    #[test]
    fn reads_a_request_that_comes_in_pieces_with_its_body() {
        let request = read_pieces(&[
            b"POST /answer HT",
            b"TP/1.1\r\nContent-Length: 10\r\n\r\n{\"seq\"",
            b":1}x",
            b"never read",
        ])
        .unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.body, b"{\"seq\":1}x");
        assert_eq!(format!("{request:?}"), "POST /answer");
    }

    #[test]
    fn says_why_a_connection_brought_no_request() {
        assert_eq!(read_pieces(&[]), Err(Nothing::Silent));
        assert_eq!(
            read_pieces(&[b"GET / HTTP/1.1\r\nHost: x"]),
            Err(Nothing::CutShort)
        );
        assert_eq!(
            read_pieces(&[b"POST /answer HTTP/1.1\r\nContent-Length: 5\r\n\r\nab"]),
            Err(Nothing::CutShort)
        );
        // A TLS hello, as a browser trying HTTPS sends.
        assert_eq!(
            read_pieces(&[b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03"]),
            Err(Nothing::NotHttp)
        );
        assert_eq!(
            read_pieces(&[b"hello there\r\n\r\n"]),
            Err(Nothing::NotHttp)
        );
        assert_eq!(
            read_pieces(&[b"GET state HTTP/1.1\r\n\r\n"]),
            Err(Nothing::NotHttp)
        );
        assert_eq!(
            read_pieces(&[b"GET / HTTP/2\r\n\r\n"]),
            Err(Nothing::NotHttp)
        );
        assert_eq!(
            read_pieces(&[b"POST / HTTP/1.1\r\nContent-Length: many\r\n\r\n"]),
            Err(Nothing::NotHttp)
        );
    }

    #[test]
    fn stops_at_a_request_too_long_to_read() {
        let long = [b'a'; 4096];
        let mut pieces: Vec<&[u8]> = vec![b"GET / HTTP/1.1\r\nX-Long: "];
        pieces.extend(std::iter::repeat_n(&long[..], 5));
        assert_eq!(read_pieces(&pieces), Err(Nothing::TooLong));
        assert_eq!(
            read_pieces(&[b"POST /answer HTTP/1.1\r\nContent-Length: 4097\r\n\r\n"]),
            Err(Nothing::TooLong)
        );
    }

    #[test]
    fn an_answer_says_its_length_and_closes() {
        let bytes = answer("200 OK", "application/json", b"{}");
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n"
        ));
        assert!(text.contains("\r\nConnection: close\r\n"));
        assert!(text.contains("\r\nCache-Control: no-store\r\n"));
        assert!(text.contains("script-src 'self';"));
        assert!(!text.contains("unsafe-inline"));
        assert!(text.ends_with("frame-ancestors 'none'\r\n\r\n{}"));
    }
}
