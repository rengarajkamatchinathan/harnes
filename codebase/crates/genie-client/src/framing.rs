//! NDJSON framing over any byte stream (02 §1): one JSON-RPC object per `\n`-terminated
//! UTF-8 line, at most [`MAX_LINE_BYTES`].

use std::io;

use futures::StreamExt;
use genie_protocol::{MAX_LINE_BYTES, Message, RpcError};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

/// Any bidirectional byte stream (socket, pipe, in-memory duplex).
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Stream for T {}

pub type BoxStream = Box<dyn Stream>;

pub struct LineReader<R> {
    inner: FramedRead<R, LinesCodec>,
}

pub struct LineWriter<W> {
    inner: W,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("line longer than {MAX_LINE_BYTES} bytes")]
    TooLong,
    /// A line that is not a valid JSON-RPC message; the connection may continue.
    #[error("bad message: {0}")]
    Bad(RpcError),
}

pub fn split<S: Stream>(s: S) -> (LineReader<ReadHalf<S>>, LineWriter<WriteHalf<S>>) {
    let (r, w) = tokio::io::split(s);
    (
        LineReader {
            inner: FramedRead::new(r, LinesCodec::new_with_max_length(MAX_LINE_BYTES)),
        },
        LineWriter { inner: w },
    )
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    /// Next message; `Ok(None)` on clean EOF.
    pub async fn next(&mut self) -> Result<Option<Message>, FrameError> {
        loop {
            match self.inner.next().await {
                None => return Ok(None),
                Some(Ok(line)) if line.trim().is_empty() => continue,
                Some(Ok(line)) => return Message::parse(&line).map(Some).map_err(FrameError::Bad),
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    return Err(FrameError::TooLong);
                }
                Some(Err(LinesCodecError::Io(e))) => return Err(FrameError::Io(e)),
            }
        }
    }
}

impl<W: AsyncWrite + Unpin> LineWriter<W> {
    pub async fn send(&mut self, msg: &Message) -> io::Result<()> {
        let mut line = msg.to_line();
        line.push('\n');
        self.inner.write_all(line.as_bytes()).await?;
        self.inner.flush().await
    }

    pub async fn send_raw_line(&mut self, line: &str) -> io::Result<()> {
        self.inner.write_all(line.as_bytes()).await?;
        self.inner.write_all(b"\n").await?;
        self.inner.flush().await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
}

/// Connect to a daemon address. `NotFound` / `ConnectionRefused` mean "not running".
pub async fn connect(addr: &crate::Address) -> io::Result<BoxStream> {
    match addr {
        #[cfg(unix)]
        crate::Address::Unix(p) => Ok(Box::new(tokio::net::UnixStream::connect(p).await?)),
        #[cfg(windows)]
        crate::Address::Pipe(name) => {
            use tokio::net::windows::named_pipe::ClientOptions;
            const ERROR_PIPE_BUSY: i32 = 231;
            let mut tries = 0;
            loop {
                match ClientOptions::new().open(name) {
                    Ok(c) => return Ok(Box::new(c)),
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 20 => {
                        tries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        #[allow(unreachable_patterns)]
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "address kind not supported on this OS",
        )),
    }
}

/// Whether a connect error means "no daemon is listening".
pub fn is_not_running(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_protocol::{Id, Request};
    use serde_json::Value;

    #[tokio::test]
    async fn roundtrip_and_limits() {
        let (a, b) = tokio::io::duplex(1024);
        let (_ra, mut wa) = split(a);
        let (mut rb, _wb) = split(b);
        wa.send(&Message::Request(Request::new(
            Id::Num(7),
            "x",
            Value::Null,
        )))
        .await
        .unwrap();
        wa.send_raw_line("not json").await.unwrap();
        assert!(
            matches!(rb.next().await.unwrap(), Some(Message::Request(r)) if r.id == Id::Num(7))
        );
        assert!(matches!(rb.next().await, Err(FrameError::Bad(e)) if e.code == -32700));
        drop(wa);
        drop(_ra);
        assert!(rb.next().await.unwrap().is_none());
    }
}
