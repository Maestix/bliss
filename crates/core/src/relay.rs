//! Moving bytes between the two legs, counting as we go.

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::tracker::Connection;

/// Per-direction buffer. Two of these per connection, so a thousand live
/// connections is ~32 MB of buffers — the point at which a pool stops being an
/// optimisation and starts being the fix. Noted rather than done: the shape of
/// the pool depends on the TUN path, which does not exist yet.
const BUFFER: usize = 16 * 1024;

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

    // Whichever direction ends first ends the connection: for a CONNECT tunnel
    // there is nothing useful left once one side is gone.
    tokio::select! {
        result = upstream => result,
        result = downstream => result,
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

        drop(client_peer);
        let _ = task.await;

        assert_eq!(conn.up(), 7, "client to server");
        assert_eq!(conn.down(), 17, "server to client");
    }
}
