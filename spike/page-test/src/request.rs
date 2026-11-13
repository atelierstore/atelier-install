//! What a browser asks over HTTP, and what the test answers.

/// The longest request head the test reads, and the longest body.
const LONGEST_HEAD: usize = 16 * 1024;
const LONGEST_BODY: usize = 4 * 1024;

/// One request: `GET /status` and its headers, or a POST with its body.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    /// The path, without what follows a `?`.
    pub path: String,
    /// The headers, their names in lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
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
    /// Its request is longer than the test reads.
    TooLong,
}

/// Reads one request: `first` is what the connection sent already, and
/// `more` waits for what it sends next and puts it in the buffer, 0 once
/// the connection is closed.
pub fn read(first: Vec<u8>, more: &mut dyn FnMut(&mut [u8]) -> usize) -> Result<Request, Nothing> {
    let mut bytes = first;
    let mut buf = [0u8; 4096];
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

/// A form's fields as a browser sends them: `first=a+b&second=c%21`.
pub fn form(body: &[u8]) -> Vec<(String, String)> {
    body.split(|&byte| byte == b'&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let mut halves = pair.splitn(2, |&byte| byte == b'=');
            let name = decoded(halves.next().unwrap_or_default());
            let value = decoded(halves.next().unwrap_or_default());
            (name, value)
        })
        .collect()
}

/// `a+b%21` as `a b!`.
fn decoded(field: &[u8]) -> String {
    let hex = |at: usize| {
        let digit = (*field.get(at)? as char).to_digit(16)?;
        Some(digit as u8)
    };
    let mut out = Vec::with_capacity(field.len());
    let mut at = 0;
    while at < field.len() {
        match (field[at], hex(at + 1), hex(at + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push(high << 4 | low);
                at += 2;
            }
            (b'+', ..) => out.push(b' '),
            (byte, ..) => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

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
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; \
         script-src 'unsafe-inline'; connect-src 'self'; img-src data:; base-uri 'none'; \
         form-action 'none'; frame-ancestors 'none'\r\n\
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

    /// Reads a request that arrives in `pieces`.
    fn read_pieces(pieces: &[&[u8]]) -> Result<Request, Nothing> {
        let mut rest = pieces.iter().skip(1);
        let first = pieces
            .first()
            .map(|piece| piece.to_vec())
            .unwrap_or_default();
        read(first, &mut |buf| match rest.next() {
            Some(piece) => {
                buf[..piece.len()].copy_from_slice(piece);
                piece.len()
            }
            None => 0,
        })
    }

    #[test]
    fn reads_a_request_and_its_headers() {
        let request = read_pieces(&[
            b"GET /status?x=1 HTTP/1.1\r\nHost: 127.0.0.1:8626\r\nX-Page-Test:  1 \r\n\r\n",
        ])
        .unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/status");
        assert_eq!(request.header("host"), Some("127.0.0.1:8626"));
        assert_eq!(request.header("x-page-test"), Some("1"));
        assert_eq!(request.header("origin"), None);
        assert!(request.body.is_empty());
    }

    #[test]
    fn reads_a_request_that_comes_in_pieces_with_its_body() {
        let request = read_pieces(&[
            b"POST /answer HT",
            b"TP/1.1\r\nContent-Length: 11\r\n\r\nfirst",
            b"=a&b=c",
            b"never read",
        ])
        .unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.body, b"first=a&b=c");
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
            read_pieces(&[b"GET status HTTP/1.1\r\n\r\n"]),
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
    fn reads_a_forms_fields() {
        assert_eq!(
            form(b"first=a+b&second=c%21%C3%A9&third"),
            [
                ("first".to_string(), "a b".to_string()),
                ("second".to_string(), "c!\u{e9}".to_string()),
                ("third".to_string(), String::new()),
            ]
        );
        assert_eq!(form(b""), []);
        // A % without its two digits stays as it is.
        assert_eq!(
            form(b"a=100%&b=%4"),
            [
                ("a".to_string(), "100%".to_string()),
                ("b".to_string(), "%4".to_string()),
            ]
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
        assert!(text.ends_with("frame-ancestors 'none'\r\n\r\n{}"));
    }
}
