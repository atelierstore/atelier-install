// This copy's WebSocket: tungstenite's blocking one, on a connection from
// the HTTP client's connector (crate::http), in place of tokio-tungstenite
// and, in browsers, ws_stream_wasm. See README.md.

use rootcause::prelude::*;
use tungstenite::{Message, WebSocket};

use crate::http::Stream;

pub enum WsMessage {
    Text(String),
    Close,
}

pub struct AppWebSocket {
    inner: WebSocket<Box<dyn Stream>>,
}

impl AppWebSocket {
    pub async fn connect(url: &str, proxy_url: Option<&str>) -> Result<Self, Report> {
        let target_url = match proxy_url {
            Some(proxy_url) => {
                format!("{proxy_url}{}", urlencoding::encode(url)).replace("https://", "wss://")
            }
            None => url.to_string(),
        };
        let uri: http::Uri = target_url
            .parse()
            .map_err(|_| report!("The provisioning socket's URL isn't one: {target_url}"))?;
        let tls = match uri.scheme_str() {
            Some("wss") => true,
            Some("ws") => false,
            _ => bail!("The provisioning socket's URL isn't a WebSocket's: {target_url}"),
        };
        let host = uri
            .host()
            .ok_or_else(|| report!("The provisioning socket's URL has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let stream = crate::http::connect(&host, port, tls).context(
            "Failed to connect to provisioning socket. Try a different anisette server.",
        )?;
        let (socket, _) = tungstenite::client::client(target_url.as_str(), stream)
            .map_err(|error| report!("The provisioning socket's handshake failed: {error}"))?;
        Ok(Self { inner: socket })
    }

    pub async fn send_text(&mut self, text: String) -> Result<(), Report> {
        self.inner.send(Message::Text(text.into()))?;
        Ok(())
    }

    /// The next text message, or `Close` once the socket is closed: never
    /// None, which the provisioning loop would wait on for good.
    pub async fn next(&mut self) -> Option<Result<WsMessage, Report>> {
        loop {
            return Some(match self.inner.read() {
                Ok(Message::Text(text)) => Ok(WsMessage::Text(text.to_string())),
                Ok(Message::Close(_)) => Ok(WsMessage::Close),
                // tungstenite answers pings itself.
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                Ok(Message::Binary(_)) => Ok(WsMessage::Close),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    Ok(WsMessage::Close)
                }
                Err(error) => Err(error.into()),
            });
        }
    }

    pub async fn close(&mut self) -> Result<(), Report> {
        match self.inner.close(None) {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    /// Polls `future` once: the socket never waits for anything else.
    fn run<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("the socket waited for something"),
        }
    }

    fn text(message: Option<Result<WsMessage, Report>>) -> Option<String> {
        match message.expect("never None").expect("no error") {
            WsMessage::Text(text) => Some(text),
            WsMessage::Close => None,
        }
    }

    #[test]
    fn talks_as_the_provisioning_session_does() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            // A ping first, which the client answers by itself.
            socket.send(Message::Ping(vec![1, 2].into())).unwrap();
            socket
                .send(Message::Text(r#"{"result":"GiveIdentifier"}"#.into()))
                .unwrap();
            let mut heard = Vec::new();
            loop {
                match socket.read() {
                    Ok(Message::Text(text)) => {
                        heard.push(text.to_string());
                        socket.close(None).unwrap();
                    }
                    Ok(Message::Pong(payload)) => {
                        heard.push(format!("pong {:?}", payload.as_ref()))
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            heard
        });

        let url = format!("ws://127.0.0.1:{port}/v3/provisioning_session");
        let mut socket = run(AppWebSocket::connect(&url, None)).unwrap();
        assert_eq!(
            text(run(socket.next())).as_deref(),
            Some(r#"{"result":"GiveIdentifier"}"#)
        );
        run(socket.send_text(r#"{"identifier":"abc"}"#.into())).unwrap();
        // The server closes, which ends the session rather than waiting.
        assert_eq!(text(run(socket.next())), None);
        assert_eq!(text(run(socket.next())), None);
        run(socket.close()).unwrap();
        assert_eq!(
            server.join().unwrap(),
            ["pong [1, 2]", r#"{"identifier":"abc"}"#]
        );
    }

    #[test]
    fn refuses_what_isnt_a_websocket() {
        assert!(run(AppWebSocket::connect("https://example.com/v3", None)).is_err());
        assert!(run(AppWebSocket::connect("not a link", None)).is_err());
    }
}
