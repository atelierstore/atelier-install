//! The HTTP client isideload's requests go through, in place of reqwest
//! (Atelier's vendored copy, see README.md).
//!
//! reqwest is built on tokio's sockets, and WASI Preview 1, where Atelier's
//! rootshell installer runs, has none. So this asks a [`Connector`] for each
//! connection, one the program sets up with [`set_connector`] (rootshell
//! lends its programs TLS connections, Network.framework's), writes one
//! HTTP/1.1 request on it with `Connection: close`, and reads the answer,
//! gzipped or not.
//! Every call blocks until it's done: it's `async` only to keep reqwest's
//! shape, so that the code calling it is isideload's own. Like reqwest, it
//! follows redirects, up to [`MAX_REDIRECTS`].
//!
//! Without a connector, `http` links go to the system's sockets, as in the
//! tests, and `https` ones fail.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};

use flate2::write::{GzDecoder, ZlibDecoder};
use std::sync::{Arc, RwLock};

pub use http::header::{self, HeaderMap, HeaderName, HeaderValue};
pub use http::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The most redirects one request follows, as reqwest's default.
pub const MAX_REDIRECTS: usize = 10;
/// The most an answer's status line and headers may take.
const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
/// What [`Response::chunk`] hands over at most at once.
const CHUNK: usize = 64 * 1024;

/// A connection to a server, already TLS's plaintext side for https.
pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Opens a connection to `host` at `port`, with TLS when `tls` is true.
pub type Connector = Arc<dyn Fn(&str, u16, bool) -> io::Result<Box<dyn Stream>> + Send + Sync>;

static CONNECTOR: RwLock<Option<Connector>> = RwLock::new(None);

/// Sets how every request connects from now on.
pub fn set_connector(connector: Connector) {
    *CONNECTOR
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(connector);
}

/// A connection through the [`Connector`], or the system's sockets.
pub(crate) fn connect(host: &str, port: u16, tls: bool) -> io::Result<Box<dyn Stream>> {
    let connector = CONNECTOR
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    match connector {
        Some(connector) => connector(host, port, tls),
        None if !tls => Ok(Box::new(std::net::TcpStream::connect((host, port))?)),
        None => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no connector for TLS: the program has to call isideload::http::set_connector",
        )),
    }
}

/// Makes requests, as reqwest's `Client` does.
#[derive(Debug, Clone, Default)]
pub struct Client {
    /// Put before every URL, which it gets URL-encoded: isideload's proxy
    /// for browsers.
    proxy: Option<String>,
}

impl Client {
    pub fn new() -> Self {
        Client::default()
    }

    pub fn with_proxy(proxy: Option<String>) -> Self {
        Client { proxy }
    }

    pub fn get(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    pub fn put(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    pub fn patch(&self, url: impl Into<String>) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    pub fn request(&self, method: Method, url: impl Into<String>) -> RequestBuilder {
        let url = url.into();
        let url = match &self.proxy {
            Some(proxy) => format!("{proxy}{}", urlencoding::encode(&url)),
            None => url,
        };
        RequestBuilder {
            method,
            url,
            headers: HeaderMap::new(),
            body: None,
            error: None,
        }
    }
}

/// A request being put together, as reqwest's `RequestBuilder`.
#[derive(Debug)]
pub struct RequestBuilder {
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Option<Vec<u8>>,
    /// The first thing that went wrong putting it together, which `send`
    /// returns.
    error: Option<Error>,
}

impl RequestBuilder {
    /// Adds a header.
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        let name = HeaderName::try_from(key).map_err(Into::into);
        let value = HeaderValue::try_from(value).map_err(Into::into);
        match (name, value) {
            (Ok(name), Ok(value)) => {
                self.headers.append(name, value);
            }
            (Err(error), _) | (_, Err(error)) => {
                self.error
                    .get_or_insert(Error::new(Kind::Builder(error.to_string())));
            }
        }
        self
    }

    /// Sets these headers, in place of any of the same names already set,
    /// as reqwest does.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        let mut last = None;
        for (name, value) in headers {
            match name {
                Some(name) => {
                    self.headers.remove(&name);
                    self.headers.append(name.clone(), value);
                    last = Some(name);
                }
                // More values of the header before.
                None => {
                    if let Some(name) = &last {
                        self.headers.append(name.clone(), value);
                    }
                }
            }
        }
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The body as JSON, with its content type unless one is set.
    pub fn json<T: Serialize + ?Sized>(mut self, json: &T) -> Self {
        match serde_json::to_vec(json) {
            Ok(body) => {
                if !self.headers.contains_key(header::CONTENT_TYPE) {
                    self.headers.insert(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                }
                self.body = Some(body);
            }
            Err(error) => {
                self.error.get_or_insert(Error::new(Kind::Json(error)));
            }
        }
        self
    }

    /// Sends the request, following redirects, and reads the answer's
    /// status and headers; its body stays to be read.
    pub async fn send(self) -> Result<Response, Error> {
        if let Some(error) = self.error {
            return Err(error.with_url(&self.url));
        }
        let RequestBuilder {
            mut method,
            mut url,
            mut headers,
            mut body,
            ..
        } = self;
        for _ in 0..=MAX_REDIRECTS {
            let target = Target::parse(&url).map_err(|error| error.with_url(&url))?;
            let response = exchange(&target, &method, &headers, body.as_deref())
                .map_err(|kind| Error::new(kind).with_url(&url))?;
            let location = response
                .headers
                .get(header::LOCATION)
                .and_then(|location| location.to_str().ok());
            let status = response.status;
            let Some(location) = location.filter(|_| status.is_redirection()) else {
                return Ok(Response { url, ..response });
            };
            let next = target.join(location);
            if status != StatusCode::TEMPORARY_REDIRECT && status != StatusCode::PERMANENT_REDIRECT
            {
                // 301, 302 and 303 go on as a GET without the body, as
                // browsers and reqwest do.
                if method != Method::HEAD {
                    method = Method::GET;
                }
                body = None;
                headers.remove(header::CONTENT_TYPE);
                headers.remove(header::CONTENT_LENGTH);
            }
            if Target::parse(&next).ok().map(|next| next.host) != Some(target.host.clone()) {
                for sensitive in [
                    header::AUTHORIZATION,
                    header::COOKIE,
                    header::PROXY_AUTHORIZATION,
                ] {
                    headers.remove(sensitive);
                }
            }
            url = next;
        }
        Err(Error::new(Kind::Redirects).with_url(&url))
    }
}

/// Where a request goes.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    tls: bool,
    host: String,
    port: u16,
    /// The path and query, from its `/`.
    path: String,
}

impl Target {
    fn parse(url: &str) -> Result<Target, Error> {
        let invalid = |why: &str| Error::new(Kind::Builder(format!("{url} {why}")));
        let uri: http::Uri = url.parse().map_err(|_| invalid("isn't a URL"))?;
        let tls = match uri.scheme_str() {
            Some("https") | Some("wss") => true,
            Some("http") | Some("ws") => false,
            _ => return Err(invalid("isn't an http or https link")),
        };
        let host = uri
            .host()
            .ok_or_else(|| invalid("has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let path = uri
            .path_and_query()
            .map(|path| path.as_str())
            .filter(|path| !path.is_empty())
            .unwrap_or("/")
            .to_string();
        Ok(Target {
            tls,
            host,
            port,
            path,
        })
    }

    /// `Host`'s value: the port only when it isn't the scheme's.
    fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == if self.tls { 443 } else { 80 } {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }

    fn origin(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.authority())
    }

    /// A redirect's `Location`, which may be relative, as a URL.
    fn join(&self, location: &str) -> String {
        if location.contains("://") {
            location.to_string()
        } else if let Some(rest) = location.strip_prefix("//") {
            let scheme = if self.tls { "https" } else { "http" };
            format!("{scheme}://{rest}")
        } else if location.starts_with('/') {
            format!("{}{location}", self.origin())
        } else {
            let path = self.path.split(['?', '#']).next().unwrap_or("/");
            let folder = &path[..path.rfind('/').map_or(0, |at| at + 1)];
            format!("{}{folder}{location}", self.origin())
        }
    }
}

/// One request and the head of its answer, on a connection of its own.
fn exchange(
    target: &Target,
    method: &Method,
    headers: &HeaderMap,
    body: Option<&[u8]>,
) -> Result<Response, Kind> {
    let stream = connect(&target.host, target.port, target.tls).map_err(Kind::Connect)?;
    let mut reader = BufReader::with_capacity(CHUNK, stream);
    // All of it in one write: each is a round trip on rootshell.
    reader
        .get_mut()
        .write_all(&request(target, method, headers, body))
        .and_then(|()| reader.get_mut().flush())
        .map_err(Kind::Io)?;
    loop {
        let (status, headers) = read_head(&mut reader)?;
        // 100 Continue, 103 Early Hints: the answer comes after.
        if status.is_informational() {
            continue;
        }
        let framing = framing(method, status, &headers)?;
        let decoder = match framing {
            Framing::Done => None,
            _ => decoder(&headers)?,
        };
        return Ok(Response {
            status,
            headers,
            url: String::new(),
            body: Body {
                reader,
                framing,
                decoder,
            },
        });
    }
}

/// The request's bytes. Header names go in title case, as reqwest sends
/// them to Apple (`http1_title_case_headers`).
fn request(target: &Target, method: &Method, headers: &HeaderMap, body: Option<&[u8]>) -> Vec<u8> {
    let mut out = format!("{method} {} HTTP/1.1\r\n", target.path).into_bytes();
    let mut line = |name: &str, value: &[u8]| {
        out.extend_from_slice(title_case(name).as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    };
    if !headers.contains_key(header::HOST) {
        line("host", target.authority().as_bytes());
    }
    for (name, value) in headers {
        if name == header::CONTENT_LENGTH || name == header::CONNECTION {
            continue;
        }
        line(name.as_str(), value.as_bytes());
    }
    if !headers.contains_key(header::ACCEPT) {
        line("accept", b"*/*");
    }
    if !headers.contains_key(header::ACCEPT_ENCODING) {
        // Nothing compressed, though Apple's developer services gzip
        // their answers all the same.
        line("accept-encoding", b"identity");
    }
    let sends_body =
        body.is_some() || matches!(*method, Method::POST | Method::PUT | Method::PATCH);
    if sends_body {
        line(
            "content-length",
            body.map_or(0, <[u8]>::len).to_string().as_bytes(),
        );
    }
    line("connection", b"close");
    out.extend_from_slice(b"\r\n");
    if let Some(body) = body {
        out.extend_from_slice(body);
    }
    out
}

fn title_case(name: &str) -> String {
    let mut upper = true;
    name.chars()
        .map(|c| {
            let c = if upper { c.to_ascii_uppercase() } else { c };
            upper = c == '-';
            c
        })
        .collect()
}

/// The answer's status line and headers.
fn read_head(reader: &mut impl BufRead) -> Result<(StatusCode, HeaderMap), Kind> {
    let mut head = Vec::new();
    loop {
        let read = reader.read_until(b'\n', &mut head).map_err(Kind::Io)?;
        if read == 0 {
            return Err(Kind::Protocol(if head.is_empty() {
                "the server closed the connection without answering".into()
            } else {
                "the server's answer ended in its headers".into()
            }));
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
        if head.len() > MAX_HEAD {
            return Err(Kind::Protocol("the server's headers are too long".into()));
        }
    }
    let mut parsed = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut response = httparse::Response::new(&mut parsed);
    match response.parse(&head) {
        Ok(httparse::Status::Complete(_)) => {}
        Ok(httparse::Status::Partial) | Err(_) => {
            return Err(Kind::Protocol("the server's answer isn't HTTP".into()));
        }
    }
    let status = response
        .code
        .and_then(|code| StatusCode::from_u16(code).ok())
        .ok_or_else(|| Kind::Protocol("the server's answer has no status".into()))?;
    let mut headers = HeaderMap::new();
    for header in response.headers.iter() {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(header.name.as_bytes()),
            HeaderValue::from_bytes(header.value),
        ) {
            headers.append(name, value);
        }
    }
    Ok((status, headers))
}

/// How the body is delimited.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Framing {
    /// This many bytes are left.
    Length(u64),
    /// In chunks; this many bytes are left of the current one, or None
    /// before a chunk's size.
    Chunked(Option<u64>),
    /// Until the server closes the connection.
    ToEnd,
    Done,
}

fn framing(method: &Method, status: StatusCode, headers: &HeaderMap) -> Result<Framing, Kind> {
    if *method == Method::HEAD
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
    {
        return Ok(Framing::Done);
    }
    let chunked = headers
        .get_all(header::TRANSFER_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("chunked"));
    if chunked {
        return Ok(Framing::Chunked(None));
    }
    match headers.get(header::CONTENT_LENGTH) {
        Some(length) => length
            .to_str()
            .ok()
            .and_then(|length| length.trim().parse().ok())
            .map(|length| {
                if length == 0 {
                    Framing::Done
                } else {
                    Framing::Length(length)
                }
            })
            .ok_or_else(|| Kind::Protocol("the server's Content-Length isn't a number".into())),
        None => Ok(Framing::ToEnd),
    }
}

/// What decompresses a body, as its Content-Encoding says it's compressed.
enum Decoder {
    Gzip(GzDecoder<Vec<u8>>),
    Zlib(ZlibDecoder<Vec<u8>>),
}

fn decoder(headers: &HeaderMap) -> Result<Option<Decoder>, Kind> {
    let Some(encoding) = headers.get(header::CONTENT_ENCODING) else {
        return Ok(None);
    };
    let encoding = encoding.to_str().unwrap_or("").trim().to_ascii_lowercase();
    match encoding.as_str() {
        "" | "identity" => Ok(None),
        "gzip" | "x-gzip" => Ok(Some(Decoder::Gzip(GzDecoder::new(Vec::new())))),
        "deflate" => Ok(Some(Decoder::Zlib(ZlibDecoder::new(Vec::new())))),
        other => Err(Kind::Protocol(format!(
            "the server compressed its answer as {other}, which this client can't read"
        ))),
    }
}

impl Decoder {
    /// What `compressed` decompresses to, as far as it goes.
    fn feed(&mut self, compressed: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Decoder::Gzip(decoder) => {
                decoder.write_all(compressed)?;
                Ok(std::mem::take(decoder.get_mut()))
            }
            Decoder::Zlib(decoder) => {
                decoder.write_all(compressed)?;
                Ok(std::mem::take(decoder.get_mut()))
            }
        }
    }

    /// The rest, once the body has ended.
    fn finish(&mut self) -> io::Result<Vec<u8>> {
        match self {
            Decoder::Gzip(decoder) => {
                decoder.try_finish()?;
                Ok(std::mem::take(decoder.get_mut()))
            }
            Decoder::Zlib(decoder) => {
                decoder.try_finish()?;
                Ok(std::mem::take(decoder.get_mut()))
            }
        }
    }
}

struct Body {
    reader: BufReader<Box<dyn Stream>>,
    framing: Framing,
    /// None once a compressed body has ended, or when it isn't compressed.
    decoder: Option<Decoder>,
}

impl fmt::Debug for Body {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Body")
            .field("framing", &self.framing)
            .finish_non_exhaustive()
    }
}

impl Body {
    /// The next bytes of the body, decompressed, or None at its end.
    fn next(&mut self) -> Result<Option<Vec<u8>>, Kind> {
        loop {
            let read = self.next_read()?;
            let Some(decoder) = &mut self.decoder else {
                return Ok(read);
            };
            let bytes = match read {
                Some(compressed) => decoder.feed(&compressed),
                None => {
                    let rest = decoder.finish();
                    self.decoder = None;
                    rest
                }
            }
            .map_err(|error| {
                Kind::Protocol(format!("the server's compressed answer is broken: {error}"))
            })?;
            if !bytes.is_empty() {
                return Ok(Some(bytes));
            }
            if self.decoder.is_none() {
                return Ok(None);
            }
        }
    }

    /// The next bytes of the body as they came, or None at its end.
    fn next_read(&mut self) -> Result<Option<Vec<u8>>, Kind> {
        loop {
            match self.framing {
                Framing::Done => return Ok(None),
                Framing::Length(left) => {
                    let bytes = self.read_some(left)?;
                    if bytes.is_empty() {
                        return Err(Kind::Protocol("the server's answer ended early".into()));
                    }
                    let left = left - bytes.len() as u64;
                    self.framing = if left == 0 {
                        Framing::Done
                    } else {
                        Framing::Length(left)
                    };
                    return Ok(Some(bytes));
                }
                Framing::ToEnd => {
                    let bytes = self.read_some(CHUNK as u64)?;
                    if bytes.is_empty() {
                        self.framing = Framing::Done;
                        return Ok(None);
                    }
                    return Ok(Some(bytes));
                }
                Framing::Chunked(None) => {
                    let size = self.chunk_size()?;
                    if size == 0 {
                        self.trailers()?;
                        self.framing = Framing::Done;
                    } else {
                        self.framing = Framing::Chunked(Some(size));
                    }
                }
                Framing::Chunked(Some(left)) => {
                    let bytes = self.read_some(left)?;
                    if bytes.is_empty() {
                        return Err(Kind::Protocol("the server's answer ended early".into()));
                    }
                    let left = left - bytes.len() as u64;
                    if left == 0 {
                        // The chunk's own line end.
                        self.line()?;
                        self.framing = Framing::Chunked(None);
                    } else {
                        self.framing = Framing::Chunked(Some(left));
                    }
                    return Ok(Some(bytes));
                }
            }
        }
    }

    /// Up to `most` bytes: what's buffered, or one read's worth.
    fn read_some(&mut self, most: u64) -> Result<Vec<u8>, Kind> {
        let buffered = self.reader.fill_buf().map_err(Kind::Io)?;
        let take = buffered.len().min(most.min(CHUNK as u64) as usize);
        let bytes = buffered[..take].to_vec();
        self.reader.consume(take);
        Ok(bytes)
    }

    fn line(&mut self) -> Result<String, Kind> {
        let mut line = Vec::new();
        let read = self
            .reader
            .by_ref()
            .take(4096)
            .read_until(b'\n', &mut line)
            .map_err(Kind::Io)?;
        if read == 0 || !line.ends_with(b"\n") {
            return Err(Kind::Protocol("the server's answer ended early".into()));
        }
        Ok(String::from_utf8_lossy(&line).trim().to_string())
    }

    fn chunk_size(&mut self) -> Result<u64, Kind> {
        let line = self.line()?;
        let size = line.split(';').next().unwrap_or("").trim();
        u64::from_str_radix(size, 16)
            .map_err(|_| Kind::Protocol("the server's answer has a broken chunk".into()))
    }

    fn trailers(&mut self) -> Result<(), Kind> {
        while !self.line()?.is_empty() {}
        Ok(())
    }
}

/// An answer, as reqwest's `Response`. Its body is read as it's asked for.
#[derive(Debug)]
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    /// Where it came from, after redirects.
    url: String,
    body: Body,
}

impl Response {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The body's length, when the server said it and it isn't compressed.
    pub fn content_length(&self) -> Option<u64> {
        if self.body.decoder.is_some() {
            return None;
        }
        match self.body.framing {
            Framing::Length(length) => Some(length),
            Framing::Done => Some(0),
            _ => None,
        }
    }

    /// An error for a 4xx or 5xx answer, as reqwest's.
    pub fn error_for_status(self) -> Result<Self, Error> {
        if self.status.is_client_error() || self.status.is_server_error() {
            Err(Error::new(Kind::Status(self.status)).with_url(&self.url))
        } else {
            Ok(self)
        }
    }

    /// The body's next bytes, or None once it's all read.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let url = &self.url;
        self.body
            .next()
            .map_err(|kind| Error::new(kind).with_url(url))
    }

    pub async fn bytes(mut self) -> Result<Vec<u8>, Error> {
        let mut all = Vec::new();
        while let Some(bytes) = self.chunk().await? {
            all.extend_from_slice(&bytes);
        }
        Ok(all)
    }

    /// The body as text; what isn't UTF-8 is replaced, as reqwest does.
    pub async fn text(self) -> Result<String, Error> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub async fn json<T: DeserializeOwned>(self) -> Result<T, Error> {
        let url = self.url.clone();
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes).map_err(|error| Error::new(Kind::Json(error)).with_url(&url))
    }
}

/// What went wrong with a request.
#[derive(Debug)]
pub struct Error {
    kind: Kind,
    url: Option<String>,
}

#[derive(Debug)]
enum Kind {
    /// The request couldn't be put together.
    Builder(String),
    /// The connection couldn't be opened.
    Connect(io::Error),
    Io(io::Error),
    /// The server's answer isn't what HTTP/1.1 says.
    Protocol(String),
    /// The server answered with an error.
    Status(StatusCode),
    Redirects,
    Json(serde_json::Error),
}

impl Error {
    fn new(kind: Kind) -> Self {
        Error { kind, url: None }
    }

    fn with_url(mut self, url: &str) -> Self {
        self.url.get_or_insert_with(|| url.to_string());
        self
    }

    /// The same error without its URL, which can hold a token.
    pub fn without_url(mut self) -> Self {
        self.url = None;
        self
    }

    /// The status of an error answer.
    pub fn status(&self) -> Option<StatusCode> {
        match self.kind {
            Kind::Status(status) => Some(status),
            _ => None,
        }
    }

    pub fn is_connect(&self) -> bool {
        matches!(self.kind, Kind::Connect(_))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            Kind::Builder(why) => write!(f, "the request can't be made: {why}")?,
            Kind::Connect(error) => write!(f, "couldn't connect: {error}")?,
            Kind::Io(error) => write!(f, "the connection broke: {error}")?,
            Kind::Protocol(why) => f.write_str(why)?,
            Kind::Status(status) => write!(f, "the server answered {status}")?,
            Kind::Redirects => write!(f, "more than {MAX_REDIRECTS} redirects")?,
            Kind::Json(error) => write!(f, "the answer isn't the JSON expected: {error}")?,
        }
        if let Some(url) = &self.url {
            write!(f, " ({url})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            Kind::Connect(error) | Kind::Io(error) => Some(error),
            Kind::Json(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// Polls `future` once: the client never waits for anything else.
    fn run<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("the client waited for something"),
        }
    }

    /// A server that answers each of `answers` to one connection, in
    /// order, and hands over each request it read.
    fn server(answers: Vec<Vec<u8>>) -> (u16, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, requests) = mpsc::channel();
        thread::spawn(move || {
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = Vec::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    request.extend_from_slice(line.as_bytes());
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                request.extend_from_slice(&body);
                // A test that doesn't look at the requests dropped the receiver.
                let _ = sent.send(request);
                stream.write_all(&answer).unwrap();
            }
        });
        (port, requests)
    }

    fn text(request: &[u8]) -> String {
        String::from_utf8(request.to_vec()).unwrap()
    }

    #[test]
    fn a_post_sends_its_headers_in_title_case_and_its_body() {
        let (port, requests) = server(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Thing: a\r\n\r\nhello".to_vec(),
        ]);
        let mut headers = HeaderMap::new();
        headers.insert("x-apple-i-md-m", HeaderValue::from_static("machine"));
        let response = run(Client::new()
            .post(format!("http://127.0.0.1:{port}/grandslam?x=1"))
            .headers(headers)
            .header("security-code", "123456".to_string())
            .body("<plist/>")
            .send())
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-thing"], "a");
        assert_eq!(response.content_length(), Some(5));
        assert_eq!(run(response.text()).unwrap(), "hello");
        let request = text(&requests.recv().unwrap());
        assert!(
            request.starts_with("POST /grandslam?x=1 HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains(&format!("Host: 127.0.0.1:{port}\r\n")));
        assert!(request.contains("X-Apple-I-Md-M: machine\r\n"));
        assert!(request.contains("Security-Code: 123456\r\n"));
        assert!(request.contains("Content-Length: 8\r\n"));
        assert!(request.contains("Connection: close\r\n"));
        assert!(request.ends_with("\r\n\r\n<plist/>"));
    }

    #[test]
    fn headers_replace_those_of_the_same_name() {
        let (port, requests) = server(vec![b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()]);
        let mut first = HeaderMap::new();
        first.insert("content-type", HeaderValue::from_static("text/x-xml-plist"));
        first.insert("x-one", HeaderValue::from_static("1"));
        let mut second = HeaderMap::new();
        second.insert("content-type", HeaderValue::from_static("application/json"));
        let response = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .headers(first)
            .headers(second)
            .send())
        .unwrap();
        assert_eq!(run(response.bytes()).unwrap(), b"");
        let request = text(&requests.recv().unwrap());
        assert!(request.contains("Content-Type: application/json\r\n"));
        assert!(!request.contains("text/x-xml-plist"));
        assert!(request.contains("X-One: 1\r\n"));
        assert!(!request.contains("Content-Length"));
    }

    #[test]
    fn reads_a_chunked_body_in_pieces_and_json() {
        let (port, _) = server(vec![
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4;ext=1\r\n{\"a\"\r\n6\r\n: 1, \"\r\n7\r\nb\": [2]\r\n1\r\n}\r\n0\r\nTrailer: x\r\n\r\n".to_vec(),
        ]);
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Answer {
            a: u32,
            b: Vec<u32>,
        }
        let response = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send())
        .unwrap();
        assert_eq!(response.content_length(), None);
        let answer: Answer = run(response.json()).unwrap();
        assert_eq!(answer, Answer { a: 1, b: vec![2] });
    }

    #[test]
    fn reads_to_the_end_without_a_length() {
        let (port, _) = server(vec![b"HTTP/1.0 200 OK\r\n\r\nall of it".to_vec()]);
        let response = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send())
        .unwrap();
        assert_eq!(run(response.text()).unwrap(), "all of it");
    }

    #[test]
    fn follows_redirects_as_a_get_and_says_where_it_ended() {
        let (port, requests) = server(vec![
            b"HTTP/1.1 302 Found\r\nLocation: /download/b.ipa\r\nContent-Length: 3\r\n\r\nnot".to_vec(),
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: c.ipa?token=1\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nipa".to_vec(),
        ]);
        let mut response = run(Client::new()
            .post(format!("http://127.0.0.1:{port}/a"))
            .body("x")
            .send())
        .unwrap();
        assert_eq!(
            response.url(),
            format!("http://127.0.0.1:{port}/download/c.ipa?token=1")
        );
        assert_eq!(run(response.chunk()).unwrap().unwrap(), b"ipa");
        assert_eq!(run(response.chunk()).unwrap(), None);
        assert!(text(&requests.recv().unwrap()).starts_with("POST /a "));
        let second = text(&requests.recv().unwrap());
        assert!(second.starts_with("GET /download/b.ipa "), "{second}");
        assert!(!second.contains("Content-Length"));
        assert!(text(&requests.recv().unwrap()).starts_with("GET /download/c.ipa?token=1 "));
    }

    #[test]
    fn an_error_status_is_an_error_when_asked() {
        let (port, _) = server(vec![
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n".to_vec(),
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ]);
        let url = format!("http://127.0.0.1:{port}/x");
        let response = run(Client::new().get(&url).send()).unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let error = run(Client::new().get(&url).send())
            .unwrap()
            .error_for_status()
            .unwrap_err();
        assert_eq!(error.status(), Some(StatusCode::NOT_FOUND));
        assert!(error.to_string().contains("404"));
        assert!(error.to_string().contains(&url));
        assert!(!error.without_url().to_string().contains(&url));
    }

    #[test]
    fn a_body_that_ends_early_is_an_error() {
        let (port, _) = server(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort".to_vec(),
        ]);
        let response = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send())
        .unwrap();
        assert!(run(response.bytes()).is_err());
    }

    fn gzipped(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn zlibbed(bytes: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// Apple's developer services gzip their plists, chunked, though the
    /// request asks for nothing compressed.
    #[test]
    fn reads_a_gzipped_or_deflated_answer() {
        let plist =
            b"<?xml version=\"1.0\"?><plist><dict><key>a</key><string>b</string></dict></plist>"
                .repeat(50);
        let gzip = gzipped(&plist);
        let mut chunked =
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n"
                .to_vec();
        for piece in gzip.chunks(7) {
            chunked.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            chunked.extend_from_slice(piece);
            chunked.extend_from_slice(b"\r\n");
        }
        chunked.extend_from_slice(b"0\r\n\r\n");
        let mut sized = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: GZIP\r\nContent-Length: {}\r\n\r\n",
            gzip.len()
        )
        .into_bytes();
        sized.extend_from_slice(&gzip);
        let deflate = zlibbed(&plist);
        let mut deflated = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: deflate\r\nContent-Length: {}\r\n\r\n",
            deflate.len()
        )
        .into_bytes();
        deflated.extend_from_slice(&deflate);
        let (port, _) = server(vec![chunked, sized, deflated]);
        for _ in 0..3 {
            let response = run(Client::new()
                .get(format!("http://127.0.0.1:{port}/"))
                .send())
            .unwrap();
            assert_eq!(response.content_length(), None);
            assert_eq!(run(response.bytes()).unwrap(), plist);
        }
    }

    #[test]
    fn refuses_what_it_cant_decompress_and_what_isnt_a_link() {
        let (port, _) = server(vec![
            b"HTTP/1.1 200 OK\r\nContent-Encoding: br\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 4\r\n\r\nnope".to_vec(),
        ]);
        let error = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send())
        .unwrap_err();
        assert!(error.to_string().contains("compressed"));
        let broken = run(Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .send())
        .unwrap();
        assert!(run(broken.bytes()).is_err());
        assert!(run(Client::new().get("ftp://example.com/").send()).is_err());
        assert!(run(Client::new().get("not a link").send()).is_err());
        let bad_header = Client::new()
            .get("http://127.0.0.1:1/")
            .header("bad header", "x");
        assert!(
            run(bad_header.send())
                .unwrap_err()
                .to_string()
                .contains("can't be made")
        );
    }

    #[test]
    fn relative_redirects_resolve_against_the_page() {
        let target = Target::parse("https://example.com/a/b.json?x=1").unwrap();
        assert_eq!(target.port, 443);
        assert_eq!(target.authority(), "example.com");
        assert_eq!(target.join("c.ipa"), "https://example.com/a/c.ipa");
        assert_eq!(target.join("/d"), "https://example.com/d");
        assert_eq!(
            target.join("//cdn.example.com/e"),
            "https://cdn.example.com/e"
        );
        assert_eq!(
            target.join("http://other.example/f"),
            "http://other.example/f"
        );
        let odd_port = Target::parse("http://example.com:8080").unwrap();
        assert_eq!(odd_port.path, "/");
        assert_eq!(odd_port.authority(), "example.com:8080");
    }

    #[test]
    fn https_goes_through_the_connector() {
        let (port, requests) = server(vec![
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ]);
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        // The other tests' http links, which may run meanwhile, go through
        // it to their own servers.
        set_connector(Arc::new(move |host, at, tls| {
            let to = if tls {
                seen.lock().unwrap().push((host.to_string(), at, tls));
                ("127.0.0.1", port)
            } else {
                (host, at)
            };
            Ok(Box::new(std::net::TcpStream::connect(to)?) as Box<dyn Stream>)
        }));
        let response = run(Client::new().get("https://gsa.apple.com/grandslam").send());
        assert_eq!(run(response.unwrap().text()).unwrap(), "ok");
        assert_eq!(
            *asked.lock().unwrap(),
            [("gsa.apple.com".to_string(), 443, true)]
        );
        assert!(text(&requests.recv().unwrap()).contains("Host: gsa.apple.com\r\n"));
    }
}
