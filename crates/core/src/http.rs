//! The HTTP half of the mixed inbound.
//!
//! Two shapes, both of which a browser sends to a proxy:
//!
//! - `CONNECT host:443` — open a tunnel and stay out of the way. Everything
//!   https goes this way, so it is nearly all of the traffic.
//! - `GET http://host/path` — an absolute URL, which is how the same browser
//!   asks for a plain-http page. This one needs the head rewritten into the
//!   origin form a web server expects, then forwarded.
//!
//! The second used to answer 501. That was defensible while the user pointed
//! their own browser at us, and stopped being defensible the moment bliss could
//! set the machine-wide proxy itself: every program on the machine would then
//! send us both shapes, and half of them would break for a reason they could
//! not see.

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

    /// The request named a path rather than a URL, so it was addressed to us as
    /// if we were the web server. Nothing is wrong with the client; it is
    /// pointed at the wrong thing.
    #[error("{0} {1} is not a proxy request: bliss is a proxy, not a web server")]
    NotProxied(String, String),

    #[error("bliss cannot proxy {0} URLs")]
    Scheme(String),

    #[error("bad target")]
    Target(#[from] TargetError),
}

/// What the client asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// A tunnel. Everything after the head is opaque.
    Connect(Target),
    /// An ordinary request for an absolute URL. `head` is already rewritten
    /// into origin form and must be sent upstream before relaying begins.
    Forward { target: Target, head: Vec<u8> },
}

impl Request {
    pub fn target(&self) -> &Target {
        match self {
            Self::Connect(t) => t,
            Self::Forward { target, .. } => target,
        }
    }
}

/// Read the request head and return where the client wants to go.
///
/// Takes only `AsyncRead`: this function does not write, and saying so lets it
/// be tested against a plain buffer instead of a socket.
pub async fn accept<S>(stream: &mut BufReader<S>) -> Result<Request, HttpError>
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
    let subject = parts
        .next()
        .ok_or_else(|| HttpError::RequestLine(line.trim_end().to_owned()))?
        .to_owned();

    // Read the headers either way. For a tunnel they are only in the way of the
    // bytes behind them; for a forwarded request they are most of what has to
    // be sent on. Leaving them unread would desynchronise the stream in both
    // cases.
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        let read = read_line_within(stream, &mut header, MAX_HEAD - total).await?;
        total += read;
        if read == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        headers.push(header.trim_end_matches(['\r', '\n']).to_owned());
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        return Ok(Request::Connect(Target::parse_authority(&subject)?));
    }

    let (target, path) = split_absolute_url(&subject, &method)?;
    Ok(Request::Forward {
        head: origin_form(&method, &path, &target, &headers),
        target,
    })
}

/// Pull the destination out of an absolute URL, leaving the path.
fn split_absolute_url(subject: &str, method: &str) -> Result<(Target, String), HttpError> {
    const SCHEME: &str = "http://";
    if !subject.to_ascii_lowercase().starts_with(SCHEME) {
        // A scheme we cannot forward is worth naming, but a bare path means the
        // request was addressed to us as a server, which is a different mistake.
        return Err(match subject.split_once("://") {
            Some((scheme, _)) => HttpError::Scheme(scheme.to_owned()),
            None => HttpError::NotProxied(method.to_owned(), subject.to_owned()),
        });
    }
    // Sliced off the original rather than the lowercased copy: the host may be
    // any case, and the path certainly matters.
    let rest = &subject[SCHEME.len()..];

    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        // `http://example.com` with nothing after it asks for the root.
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(HttpError::Target(TargetError::EmptyHost));
    }

    // Unlike CONNECT, a URL's port is optional, and http means 80.
    let target = match Target::parse_authority(authority) {
        Ok(t) => t,
        Err(_) => Target::domain(authority.trim_start_matches('[').trim_end_matches(']'), 80),
    };
    Ok((target, path.to_owned()))
}

/// Rebuild the head the way an origin server expects it.
///
/// Three changes, each with a reason:
///
/// - the request line carries the path, not the URL — an origin server that is
///   handed an absolute URL is entitled to reject it;
/// - `Proxy-Connection` and `Proxy-Authorization` are between the client and
///   us, and must not travel any further;
/// - `Connection: close` goes on, because the next request on a kept-alive
///   client connection may be for a different host, and this relay has already
///   dialled exactly one. Closing is the honest way to say so; pretending to
///   keep the connection alive would send the second request to the first
///   request's server.
fn origin_form(method: &str, path: &str, target: &Target, headers: &[String]) -> Vec<u8> {
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    let mut seen_host = false;

    for header in headers {
        let name = header.split(':').next().unwrap_or_default().trim();
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("keep-alive")
        {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            seen_host = true;
        }
        head.push_str(header);
        head.push_str("\r\n");
    }

    // HTTP/1.1 requires it, and an ancient client that left it out would
    // otherwise reach a virtual host that cannot tell which site it wants.
    if !seen_host {
        match target {
            Target::Domain { host, port: 80 } => head.push_str(&format!("Host: {host}\r\n")),
            other => head.push_str(&format!("Host: {other}\r\n")),
        }
    }

    head.push_str("Connection: close\r\n\r\n");
    head.into_bytes()
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
///
/// The body is Russian more often than not, and so is the system's own text
/// for an error (Windows words its socket errors in the user's language):
/// without a charset a browser read it in the locale's legacy code page, and
/// a friend's first look at a refusal was a line of «РЎРґРµР»Р°РЅР°…».
pub async fn refuse<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: &str,
    detail: &str,
) -> std::io::Result<()> {
    let body = format!("bliss: {detail}\n");
    stream
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    /// A refusal says its text is UTF-8, or a browser shows Russian as
    /// mojibake in the locale's code page (seen on a friend's Windows).
    #[tokio::test]
    async fn a_refusal_names_its_charset() {
        let mut out = Vec::new();
        refuse(&mut out, "502 Bad Gateway", "сеть недоступна")
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(
            head.contains("Content-Type: text/plain; charset=utf-8"),
            "{head}"
        );
        assert!(
            head.contains(&format!("Content-Length: {}", body.len())),
            "{head}"
        );
        assert_eq!(body, "bliss: сеть недоступна\n");
    }

    async fn head(bytes: Vec<u8>) -> Result<Request, HttpError> {
        accept(&mut BufReader::new(std::io::Cursor::new(bytes))).await
    }

    /// The head as it would go upstream, as text, for readable assertions.
    async fn forwarded(bytes: Vec<u8>) -> (Target, String) {
        match head(bytes).await.expect("a forwardable request") {
            Request::Forward { target, head } => {
                (target, String::from_utf8(head).expect("utf-8 head"))
            }
            other => panic!("expected a forward, got {other:?}"),
        }
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
            Request::Connect(Target::domain("example.com", 443))
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
            Request::Connect(Target::domain("example.com", 443))
        );
    }

    /// The shape a browser sends for a plain-http page once the machine-wide
    /// proxy points at us. It has to come out as an origin-form request.
    #[tokio::test]
    async fn an_absolute_url_becomes_an_origin_form_request() {
        let mut request = b"GET http://example.com/a/b?q=1 HTTP/1.1\r\n".to_vec();
        request.extend_from_slice(b"Host: example.com\r\n");
        request.extend_from_slice(b"User-Agent: curl/8\r\n");
        request.extend_from_slice(b"Proxy-Connection: keep-alive\r\n");
        request.extend_from_slice(b"\r\n");

        let (target, head) = forwarded(request).await;
        // http means port 80 when the URL does not say.
        assert_eq!(target, Target::domain("example.com", 80));

        assert!(
            head.starts_with("GET /a/b?q=1 HTTP/1.1\r\n"),
            "got {head:?}"
        );
        assert!(head.contains("Host: example.com\r\n"));
        assert!(head.contains("User-Agent: curl/8\r\n"));
        // Between the client and us, and no further.
        assert!(!head.contains("Proxy-Connection"));
        // One request per dial, said out loud rather than implied.
        assert!(head.contains("Connection: close\r\n"));
        assert!(head.ends_with("\r\n\r\n"), "the head must be terminated");
    }

    #[tokio::test]
    async fn a_url_with_a_port_and_no_path_still_works() {
        let mut request = b"POST http://Example.COM:8080 HTTP/1.1\r\n".to_vec();
        request.extend_from_slice(b"Content-Length: 3\r\n\r\n");

        let (target, head) = forwarded(request).await;
        assert_eq!(target, Target::domain("Example.COM", 8080));
        // No path in the URL means the root.
        assert!(head.starts_with("POST / HTTP/1.1\r\n"), "got {head:?}");
        // The body's length has to survive, or the request is truncated.
        assert!(head.contains("Content-Length: 3\r\n"));
    }

    /// A client that leaves Host out gets one, because a shared host cannot
    /// tell which site the request is for without it.
    #[tokio::test]
    async fn a_missing_host_header_is_supplied() {
        let mut request = b"GET http://example.com/x HTTP/1.0\r\n".to_vec();
        request.extend_from_slice(b"\r\n");
        let (_, head) = forwarded(request).await;
        assert!(head.contains("Host: example.com\r\n"), "got {head:?}");

        // With a non-default port the header has to carry it too.
        let mut request = b"GET http://example.com:8080/x HTTP/1.0\r\n".to_vec();
        request.extend_from_slice(b"\r\n");
        let (_, head) = forwarded(request).await;
        assert!(head.contains("Host: example.com:8080\r\n"), "got {head:?}");
    }

    /// A request aimed at us as if we were the web server is a different
    /// mistake from an unsupported method, and says so.
    #[tokio::test]
    async fn a_request_without_a_url_is_refused_as_not_proxied() {
        let mut request = b"GET /index.html HTTP/1.1\r\n".to_vec();
        request.extend_from_slice(b"\r\n");
        let err = head(request).await.expect_err("not a proxy request");
        assert!(
            matches!(&err, HttpError::NotProxied(m, s) if m == "GET" && s == "/index.html"),
            "got {err:?}"
        );

        let mut request = b"GET ftp://example.com/f HTTP/1.1\r\n".to_vec();
        request.extend_from_slice(b"\r\n");
        let err = head(request).await.expect_err("not http");
        assert!(
            matches!(&err, HttpError::Scheme(s) if s == "ftp"),
            "got {err:?}"
        );
    }
}
