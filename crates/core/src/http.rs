//! The HTTP half of the mixed inbound.
//!
//! CONNECT only. Absolute-form requests (`GET http://host/path`) are the other
//! half of an HTTP proxy and are not served yet: almost everything worth
//! tunnelling is https, and a client that tries gets a clear 501 rather than a
//! hang. Worth adding when plain-http sites turn out to matter.

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::target::{Target, TargetError};

/// Enough for any sane request head; beyond it we are being fed garbage.
const MAX_HEAD: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("io")]
    Io(#[from] std::io::Error),

    #[error("request head is larger than {MAX_HEAD} bytes")]
    HeadTooLarge,

    #[error("malformed request line {0:?}")]
    RequestLine(String),

    #[error("bliss serves CONNECT only, got {0}")]
    Method(String),

    #[error("bad target")]
    Target(#[from] TargetError),
}

/// Read the request head and return where the client wants to go.
pub async fn accept<S>(stream: &mut BufReader<S>) -> Result<Target, HttpError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut line = String::new();
    let mut read = stream.read_line(&mut line).await?;
    if read == 0 {
        return Err(HttpError::RequestLine(String::new()));
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let authority = parts
        .next()
        .ok_or_else(|| HttpError::RequestLine(line.trim_end().to_owned()))?
        .to_owned();

    // Drain the headers either way: leaving them unread would desynchronise the
    // stream that is about to become a tunnel.
    let mut total = read;
    loop {
        let mut header = String::new();
        read = stream.read_line(&mut header).await?;
        total += read;
        if total > MAX_HEAD {
            return Err(HttpError::HeadTooLarge);
        }
        if read == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }

    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(HttpError::Method(method));
    }
    Ok(Target::parse_authority(&authority)?)
}

pub async fn established<S: AsyncWrite + Unpin>(stream: &mut S) -> std::io::Result<()> {
    stream
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;
    stream.flush().await
}

/// Refuse in a way a browser will show rather than spin on.
pub async fn refuse<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: &str,
    detail: &str,
) -> std::io::Result<()> {
    let body = format!("bliss: {detail}\n");
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    stream.flush().await
}
