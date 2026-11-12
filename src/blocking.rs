//! idevice's pairing host is async, on tokio's I/O traits, and rootshell's
//! sockets block. A blocking socket is an async one that's always ready, so
//! this dresses one up as such, and runs a future on it to the end without a
//! runtime: tokio's runtime has no place on WASI Preview 1, and nothing here
//! ever has to wait for two things at once.

use std::future::Future;
use std::io::{self, Read, Write};
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A blocking stream, as tokio's async traits.
#[derive(Debug)]
pub struct Blocking<S>(pub S);

impl<S: Read + Unpin> AsyncRead for Blocking<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let n = self.0.read(buf.initialize_unfilled())?;
        buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl<S: Write + Unpin> AsyncWrite for Blocking<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(self.0.write(buf))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.flush())
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Runs `future` to its end on this thread. Everything it waits for must be
/// a [`Blocking`] stream, which is ready whenever it's asked; a future that
/// says it's waiting for something else is one this can't run.
pub fn run<F: Future>(future: F) -> Result<F::Output, NotReady> {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    // A future may hand back to its caller between two ready steps, so one
    // "not ready" isn't the end; a run of them is.
    for _ in 0..1000 {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return Ok(output);
        }
    }
    Err(NotReady)
}

/// A future waited for something that isn't a blocking stream.
#[derive(Debug)]
pub struct NotReady;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn async_reads_and_writes_run_on_a_blocking_stream() {
        let mut stream = Blocking(io::Cursor::new(b"hello".to_vec()));
        let text = run(async {
            let mut text = [0u8; 5];
            stream.read_exact(&mut text).await.unwrap();
            stream.0.set_position(0);
            stream.write_all(b"HE").await.unwrap();
            stream.flush().await.unwrap();
            text
        })
        .unwrap();
        assert_eq!(&text, b"hello");
        assert_eq!(stream.0.into_inner(), b"HEllo");
    }

    #[test]
    fn a_future_that_waits_for_something_else_ends_the_run() {
        assert!(run(std::future::pending::<()>()).is_err());
    }
}
