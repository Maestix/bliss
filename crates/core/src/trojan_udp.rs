//! Datagrams to the engine over a Trojan stream on loopback.
//!
//! A SOCKS5 UDP association is a UDP port on the engine, and a UDP port takes
//! datagrams from anyone: while one association was open, any other app on the
//! phone could send a SOCKS datagram to that port with no login at all and
//! leave through the server around bliss's rules, which is how a program finds
//! out where the tunnel comes out (checked against xray 26.3.27, which also
//! turns every datagram it was not asked for away, but not those). A phone's
//! loopback is everyone's, so the datagrams go another way: xray's Trojan
//! inbound carries UDP inside its TCP stream, behind a password, and opens no
//! UDP socket at all. This file is the client end of that, as much of Trojan
//! as a loopback hop needs: the key line, `UDP ASSOCIATE`, and the framed
//! datagram, in both directions.

use std::net::SocketAddr;

use async_trait::async_trait;
use sha2::{Digest, Sha224};
use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use crate::outbound::{BoxedDatagram, Datagram};
use crate::socks5::{read_target, write_target};
use crate::target::Target;

/// Trojan's command for a stream of datagrams.
const UDP_ASSOCIATE: u8 = 3;
const CRLF: &[u8] = b"\r\n";
/// A datagram's length is two bytes in the frame.
const MAX_PAYLOAD: usize = 65_535;
/// Datagrams read from the engine and not yet taken: past this the stream is
/// not read, and the engine's own buffers take the weight.
const QUEUE: usize = 128;

/// The first line of every Trojan connection: the password's SHA-224, in hex.
pub(crate) fn key_line(password: &str) -> Vec<u8> {
    let digest = Sha224::digest(password.as_bytes());
    let mut line = Vec::with_capacity(58);
    for byte in digest {
        line.extend_from_slice(format!("{byte:02x}").as_bytes());
    }
    line.extend_from_slice(CRLF);
    line
}

/// One framed datagram to `to`.
pub(crate) fn frame(to: &Target, payload: &[u8]) -> std::io::Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a datagram too long for one frame",
        ));
    }
    let mut out = Vec::with_capacity(payload.len() + 32);
    write_target(&mut out, to).map_err(std::io::Error::from)?;
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(CRLF);
    out.extend_from_slice(payload);
    Ok(out)
}

/// Open a stream of datagrams to the engine's Trojan inbound at `addr`.
pub(crate) async fn open(
    addr: SocketAddr,
    password: &str,
    to: &Target,
) -> std::io::Result<BoxedDatagram> {
    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    let (mut reader, mut writer) = tokio::io::split(stream);

    // The destination named here is a formality: each frame carries its own.
    let mut head = key_line(password);
    head.push(UDP_ASSOCIATE);
    write_target(
        &mut head,
        &Target::Socket(SocketAddr::from(([0, 0, 0, 0], 0))),
    )
    .map_err(std::io::Error::from)?;
    head.extend_from_slice(CRLF);
    writer.write_all(&head).await?;

    // The frames are read by a task of their own, so that a `recv` given up
    // halfway (a select that took the other branch) never leaves the stream
    // in the middle of one.
    let (queue, incoming) = mpsc::channel::<Vec<u8>>(QUEUE);
    let reading = tokio::spawn(async move {
        loop {
            // The address the answer came from, which the flow already knows.
            if read_target(&mut reader).await.is_err() {
                return;
            }
            let mut size = [0u8; 4];
            if reader.read_exact(&mut size).await.is_err() {
                return;
            }
            let length = u16::from_be_bytes([size[0], size[1]]) as usize;
            if &size[2..] != CRLF {
                return;
            }
            let mut payload = vec![0u8; length];
            if reader.read_exact(&mut payload).await.is_err() {
                return;
            }
            if queue.send(payload).await.is_err() {
                return;
            }
        }
    });

    Ok(Box::new(TrojanDatagram {
        writer: Mutex::new(writer),
        incoming: Mutex::new(incoming),
        reading,
        to: to.clone(),
    }))
}

struct TrojanDatagram {
    writer: Mutex<WriteHalf<TcpStream>>,
    incoming: Mutex<mpsc::Receiver<Vec<u8>>>,
    /// Ended with the flow.
    reading: JoinHandle<()>,
    to: Target,
}

impl Drop for TrojanDatagram {
    fn drop(&mut self) {
        self.reading.abort();
    }
}

#[async_trait]
impl Datagram for TrojanDatagram {
    async fn send(&self, payload: &[u8]) -> std::io::Result<()> {
        let framed = frame(&self.to, payload)?;
        self.writer.lock().await.write_all(&framed).await
    }

    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let Some(payload) = self.incoming.lock().await.recv().await else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "the engine closed the stream of datagrams",
            ));
        };
        // A datagram longer than the buffer is cut, as a socket cuts it.
        let n = payload.len().min(buf.len());
        buf[..n].copy_from_slice(&payload[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_the_hex_of_a_sha224() {
        // SHA-224("abc"), from FIPS 180-4's examples.
        assert_eq!(
            key_line("abc"),
            b"23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7\r\n".to_vec()
        );
    }

    #[test]
    fn a_frame_is_the_address_the_length_a_line_end_and_the_payload() {
        let to = Target::Socket("203.0.113.9:53".parse().unwrap());
        let framed = frame(&to, b"hi").unwrap();
        assert_eq!(
            framed,
            [&[1, 203, 0, 113, 9, 0, 53, 0, 2, 13, 10][..], b"hi"].concat()
        );
        let name = Target::Domain {
            host: "a.example".to_owned(),
            port: 443,
        };
        let framed = frame(&name, b"").unwrap();
        assert_eq!(&framed[..2], &[3, 9]);
        assert_eq!(&framed[framed.len() - 4..], &[0, 0, 13, 10]);
        assert!(frame(&to, &vec![0u8; MAX_PAYLOAD + 1]).is_err());
        assert!(frame(&to, &vec![0u8; MAX_PAYLOAD]).is_ok());
    }
}
