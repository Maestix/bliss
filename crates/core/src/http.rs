//! The HTTP half of the mixed inbound.
//!
//! CONNECT only. Absolute-form requests (`GET http://host/path`) are the other
//! half of an HTTP proxy and are not served yet: almost everything worth
//! tunnelling is https, and a client that tries gets a clear 501 rather than a
//! hang. Worth adding when plain-http sites turn out to matter.

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

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
///
/// Takes only `AsyncRead`: this function does not write, and saying so lets it
/// be tested against a plain buffer instead of a socket.
pub async fn accept<S>(stream: &mut BufReader<S>) -> Result<Target, HttpError>
where
    S: AsyncRead + Unpin,
{
    let mut line = String::new();
    let mut total = read_line_within(stream, &mut line, MAX_HEAD).await?;
    if total == 0 {
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
    loop {
        let mut header = String::new();
        let read = read_line_within(stream, &mut header, MAX_HEAD - total).await?;
        total += read;
        if read == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }

    if !method.eq_ignore_ascii_case("CONNECT") {
        return Err(HttpError::Method(method));
    }
    Ok(Target::parse_authority(&authority)?)
}

/// Read one line, refusing to buffer more than `budget` bytes of it.
///
/// `read_line` has no limit of its own: it grows the string until it meets a
/// newline or the peer stops talking. MAX_HEAD used to be tested only between
/// lines, so it bounded how *many* lines a peer could send and not how long one
/// of them could be — and one line with no newline in it was unbounded memory
/// on a connection anything local could open. The constant's own comment
/// promised the opposite of what the code did.
async fn read_line_within<S>(
    stream: &mut BufReader<S>,
    out: &mut String,
    budget: usize,
) -> Result<usize, HttpError>
where
    S: AsyncRead + Unpin,
{
    if budget == 0 {
        return Err(HttpError::HeadTooLarge);
    }

    // `take` bounds the bytes that can reach the string, so the allocation is
    // capped before it happens rather than measured afterwards.
    let mut limited = (&mut *stream).take(budget as u64);
    let read = limited.read_line(out).await?;

    // Spent the whole budget without reaching the end of the line: whatever is
    // on the other end is not sending an HTTP head, and we stop reading it.
    if read == budget && !out.ends_with('\n') {
        return Err(HttpError::HeadTooLarge);
    }
    Ok(read)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn head(bytes: Vec<u8>) -> Result<Target, HttpError> {
        accept(&mut BufReader::new(std::io::Cursor::new(bytes))).await
    }

    const OPENING: &[u8] = b"CONNECT example.com:443 HTTP/1.1\r\n";

    #[tokio::test]
    async fn reads_an_ordinary_connect() {
        let mut request = OPENING.to_vec();
        request.extend_from_slice(b"Host: example.com:443\r\n");
        request.extend_from_slice(b"Proxy-Connection: keep-alive\r\n");
        request.extend_from_slice(b"\r\n");
        assert_eq!(
            head(request).await.expect("a plain CONNECT"),
            Target::domain("example.com", 443)
        );
    }

    /// The reason `read_line_within` exists. One line, no newline anywhere in
    /// it: the old code buffered every byte before it looked at the limit.
    #[tokio::test]
    async fn one_endless_line_is_refused_rather_than_buffered() {
        let flood = vec![b'A'; MAX_HEAD * 4];
        assert!(matches!(head(flood).await, Err(HttpError::HeadTooLarge)));
    }

    /// The limit the old code did enforce still has to hold: many short lines
    /// must not add up past the cap either.
    #[tokio::test]
    async fn many_short_headers_still_hit_the_cap() {
        let mut request = OPENING.to_vec();
        while request.len() < MAX_HEAD * 2 {
            request.extend_from_slice(b"X-Filler: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        request.extend_from_slice(b"\r\n");
        assert!(matches!(head(request).await, Err(HttpError::HeadTooLarge)));
    }

    /// A head that ends exactly on the cap is legal, and must not be refused
    /// for arriving in one piece.
    #[tokio::test]
    async fn a_head_just_inside_the_cap_is_accepted() {
        let mut request = OPENING.to_vec();
        let room = MAX_HEAD - request.len() - "X-Filler: \r\n".len() - "\r\n".len();
        request.extend_from_slice(b"X-Filler: ");
        request.extend(std::iter::repeat_n(b'a', room));
        request.extend_from_slice(b"\r\n");
        request.extend_from_slice(b"\r\n");
        assert_eq!(request.len(), MAX_HEAD, "exactly on the limit");

        assert_eq!(
            head(request).await.expect("inside the cap"),
            Target::domain("example.com", 443)
        );
    }

    #[tokio::test]
    async fn a_method_we_do_not_serve_is_named_in_the_error() {
        let mut request = b"GET http://example.com/ HTTP/1.1\r\n".to_vec();
        request.extend_from_slice(b"\r\n");
        let err = head(request).await.expect_err("not CONNECT");
        assert!(matches!(err, HttpError::Method(m) if m == "GET"));
    }
}
