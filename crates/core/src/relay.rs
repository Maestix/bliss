//! Moving bytes between the two legs, counting as we go.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::tracker::Connection;

/// Per-direction buffer. Two of these per connection, so a thousand live
/// connections is ~32 MB of buffers — the point at which a pool stops being an
/// optimisation and starts being the fix. Noted rather than done, and the TUN
/// path takes two more per flow of datagrams, so the number to measure it
/// against is now connections plus flows.
const BUFFER: usize = 16 * 1024;

/// Per-direction buffer for a flow of datagrams, and therefore the largest one
/// bliss carries: a bigger datagram would be cut, and cutting one is worse
/// than refusing it. Nothing that arrives through an adapter with a 1500-byte
/// MTU comes close, and a flow costs two of these rather than one.
const DATAGRAM: usize = 16 * 1024;

/// Pump both directions until either side closes, keeping the connection's
/// counters current as the bytes pass.
pub async fn relay<C, S>(client: C, server: S, conn: Arc<Connection>) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin + Send,
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (mut client_read, mut client_write) = tokio::io::split(client);
    let (mut server_read, mut server_write) = tokio::io::split(server);

    let upstream = {
        let conn = Arc::clone(&conn);
        pump(&mut client_read, &mut server_write, move |n| conn.add_up(n))
    };
    let downstream = pump(&mut server_read, &mut client_write, move |n| {
        conn.add_down(n)
    });

    // Both directions run to the end; only a failure cuts the other one short.
    //
    // This used to be a `select!`, which returned the moment either direction
    // finished and dropped the other wherever it was parked — possibly holding
    // bytes it had already read. That made `pump`'s half-close meaningless: the
    // FIN was followed microseconds later by a close, so the peer never got the
    // chance the half-close exists to grant it. A client that sent its request
    // and shut down its writing half received nothing at all.
    //
    // `try_join!` waits for both on the happy path and abandons the other on the
    // first error, which is the right instinct in the one case where the
    // connection really is beyond use.
    //
    // The cost is that a peer which half-closes and then never hears back holds
    // the connection open until the far end closes. That is ordinary TCP and
    // every proxy has it; the answer is an idle timeout, which bliss does not
    // have yet.
    tokio::try_join!(upstream, downstream).map(|_| ())
}

/// One datagram per read, one per write, for as long as the flow lives.
///
/// Apart from `relay` because a datagram has edges. A stream's buffer may hold
/// half of the next message and nobody minds; here half a message is a broken
/// packet, so nothing is ever split or joined, and a buffer too small to hold
/// one would silently cut it.
///
/// It also ends differently: a flow is over when the program stops sending,
/// because the far side has no way to say it has finished. Whichever direction
/// stops first ends the other, which for `relay` would truncate a response and
/// here is the only way out.
pub async fn relay_datagrams<C>(
    client: C,
    server: crate::outbound::BoxedDatagram,
    conn: Arc<Connection>,
) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (mut from_client, mut to_client) = tokio::io::split(client);
    let server = &*server;

    let out = {
        let conn = Arc::clone(&conn);
        async move {
            let mut buf = vec![0u8; DATAGRAM];
            loop {
                let n = from_client.read(&mut buf).await?;
                if n == 0 {
                    return Ok::<_, std::io::Error>(());
                }
                server.send(&buf[..n]).await?;
                conn.add_up(n as u64);
            }
        }
    };

    let back = async move {
        let mut buf = vec![0u8; DATAGRAM];
        loop {
            let n = server.recv(&mut buf).await?;
            to_client.write_all(&buf[..n]).await?;
            conn.add_down(n as u64);
        }
    };

    tokio::select! {
        result = out => result,
        result = back => result,
    }
}

async fn pump<R, W>(
    reader: &mut R,
    writer: &mut W,
    mut counted: impl FnMut(u64),
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; BUFFER];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            // Half-close rather than drop: the peer may still have something to
            // send, and a hard drop would truncate it.
            let _ = writer.shutdown().await;
            return Ok(());
        }
        writer.write_all(&buf[..n]).await?;
        counted(n as u64);
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::target::Target;
    use crate::tracker::{Route, Tracker};

    /// Both directions must be counted. The connections screen is only worth
    /// having if the numbers in it are real.
    #[tokio::test]
    async fn counts_both_directions() {
        let (client, mut client_peer) = tokio::io::duplex(1024);
        let (server, mut server_peer) = tokio::io::duplex(1024);

        let tracker = Tracker::new();
        let conn = tracker.open(Target::domain("x.example", 443), Route::Proxy, "test", None);

        let task = tokio::spawn(relay(client, server, Arc::clone(&conn)));

        client_peer.write_all(b"request").await.unwrap();
        let mut seen = [0u8; 7];
        server_peer.read_exact(&mut seen).await.unwrap();
        assert_eq!(&seen, b"request");

        server_peer.write_all(b"a-longer-response").await.unwrap();
        let mut back = [0u8; 17];
        client_peer.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"a-longer-response");

        // Both ends go, which is what a closed socket looks like. Dropping only
        // the client used to be enough to end the relay — that was the bug in
        // the test below this one, written into an assertion.
        drop(client_peer);
        drop(server_peer);
        let _ = task.await;

        assert_eq!(conn.up(), 7, "client to server");
        assert_eq!(conn.down(), 17, "server to client");
    }

    /// A client that says its piece and half-closes must still get the answer.
    ///
    /// This is the shape of every request-then-listen protocol, and of
    /// `nc -N` and friends through the SOCKS port. `pump` half-closes rather
    /// than dropping precisely so the peer can keep sending; the tear-down has
    /// to honour that or the courtesy is theatre.
    #[tokio::test]
    async fn a_half_close_does_not_cancel_the_other_direction() {
        let (client, mut client_peer) = tokio::io::duplex(1024);
        let (server, mut server_peer) = tokio::io::duplex(1024);

        let tracker = Tracker::new();
        let conn = tracker.open(Target::domain("x.example", 80), Route::Proxy, "test", None);
        let task = tokio::spawn(relay(client, server, Arc::clone(&conn)));

        // Say everything, then close only the sending half.
        client_peer.write_all(b"request").await.unwrap();
        client_peer.shutdown().await.unwrap();

        let mut seen = [0u8; 7];
        server_peer.read_exact(&mut seen).await.unwrap();
        assert_eq!(&seen, b"request");

        // The half-close has to arrive as an end of stream, not as a dead
        // socket. Reading it here also pins the ordering: by the time the
        // response is written, the upstream direction has certainly finished.
        let mut nothing = [0u8; 1];
        assert_eq!(
            server_peer.read(&mut nothing).await.unwrap(),
            0,
            "half-close"
        );

        server_peer.write_all(b"the answer").await.unwrap();
        drop(server_peer);

        let mut answer = Vec::new();
        client_peer.read_to_end(&mut answer).await.unwrap();
        assert_eq!(
            answer, b"the answer",
            "the response outlives the half-close"
        );

        let _ = task.await;
        assert_eq!(conn.down(), 10);
    }

    /// A flow of datagrams is carried both ways and counted like a connection,
    /// so a call shows up in the table with the rest of the traffic rather
    /// than as nothing at all.
    #[tokio::test]
    async fn a_flow_of_datagrams_is_carried_and_counted() {
        struct Fake {
            sent: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
            back: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
        }

        #[async_trait::async_trait]
        impl crate::outbound::Datagram for Fake {
            async fn send(&self, payload: &[u8]) -> std::io::Result<()> {
                let _ = self.sent.send(payload.to_vec());
                Ok(())
            }
            async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
                let Some(next) = self.back.lock().await.recv().await else {
                    return Err(std::io::Error::other("the flow ended"));
                };
                buf[..next.len()].copy_from_slice(&next);
                Ok(next.len())
            }
        }

        let (sent, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let (answer, back) = tokio::sync::mpsc::unbounded_channel();
        let fake = Fake {
            sent,
            back: tokio::sync::Mutex::new(back),
        };

        let (client, mut client_peer) = tokio::io::duplex(4096);
        let tracker = Tracker::new();
        let conn = tracker.open(
            Target::Socket("203.0.113.9:443".parse().unwrap()),
            Route::Proxy,
            "test",
            None,
        );
        let task = tokio::spawn(relay_datagrams(client, Box::new(fake), Arc::clone(&conn)));

        client_peer.write_all(b"a request").await.unwrap();
        assert_eq!(arrived.recv().await.unwrap(), b"a request");

        // Only now, so the two answers cannot arrive as one read and the test
        // is about edges rather than about timing.
        answer.send(b"first".to_vec()).unwrap();
        let mut seen = [0u8; 5];
        client_peer.read_exact(&mut seen).await.unwrap();
        assert_eq!(&seen, b"first");
        answer.send(b"second".to_vec()).unwrap();
        let mut seen = [0u8; 6];
        client_peer.read_exact(&mut seen).await.unwrap();
        assert_eq!(&seen, b"second");

        // The flow ends when the program stops sending, which is the only end
        // a datagram has.
        drop(client_peer);
        let _ = task.await;
        assert_eq!(conn.up(), 9);
        assert_eq!(conn.down(), 11);
    }
}
