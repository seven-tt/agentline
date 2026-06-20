//! Connection authentication (challenge-response) and HMAC-signed framing.
//!
//! Protocol:
//! 1. Server sends 32-byte random nonce
//! 2. Client responds with HMAC-SHA256(key, nonce)  (32 bytes)
//! 3. Server verifies → proceed or disconnect
//!
//! After handshake, all data flows through signed frames:
//!   [4-byte payload length BE][32-byte HMAC-SHA256][payload]

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::TransportConn;

type HmacSha256 = Hmac<Sha256>;

const NONCE_LEN: usize = 32;
const HMAC_LEN: usize = 32;
const LEN_SIZE: usize = 4;
const HEADER_SIZE: usize = LEN_SIZE + HMAC_LEN;
const MAX_FRAME: usize = 16 * 1024 * 1024; // 16 MB

fn derive_key(token: &str) -> [u8; 32] {
    use sha2::Digest;
    let hash = Sha256::digest(token.as_bytes());
    hash.into()
}

fn compute_hmac(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().into()
}

// ── Handshake ───────────────────────────────────────────────────────────

async fn server_handshake<R, W>(key: &[u8; 32], r: &mut R, w: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let nonce: [u8; NONCE_LEN] = rand::random();
    w.write_all(&nonce).await?;
    w.flush().await?;

    let mut response = [0u8; HMAC_LEN];
    r.read_exact(&mut response).await?;

    let expected = compute_hmac(key, &nonce);
    if response != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "token verification failed",
        ));
    }
    Ok(())
}

/// Authenticate and wrap a connection with HMAC-signed framing.
/// Returns a new `TransportConn` whose read/write are signed.
pub async fn wrap_authenticated(token: &str, mut conn: TransportConn) -> io::Result<TransportConn> {
    let key = derive_key(token);
    server_handshake(&key, &mut conn.read, &mut conn.write).await?;
    Ok(TransportConn {
        protocol: conn.protocol,
        read: Box::new(SignedReader::new(conn.read, key)),
        write: Box::new(SignedWriter::new(conn.write, key)),
    })
}

// ── SignedReader ─────────────────────────────────────────────────────────

enum ReadState {
    ReadingHeader,
    ReadingPayload,
    Yielding,
}

pub struct SignedReader<R> {
    inner: R,
    key: [u8; 32],
    header: [u8; HEADER_SIZE],
    header_filled: usize,
    payload: Vec<u8>,
    payload_filled: usize,
    payload_len: usize,
    out_pos: usize,
    state: ReadState,
}

impl<R> SignedReader<R> {
    fn new(inner: R, key: [u8; 32]) -> Self {
        Self {
            inner,
            key,
            header: [0; HEADER_SIZE],
            header_filled: 0,
            payload: Vec::new(),
            payload_filled: 0,
            payload_len: 0,
            out_pos: 0,
            state: ReadState::ReadingHeader,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for SignedReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        loop {
            match me.state {
                ReadState::Yielding => {
                    if me.out_pos >= me.payload_len {
                        me.state = ReadState::ReadingHeader;
                        me.header_filled = 0;
                        continue;
                    }
                    let remaining = &me.payload[me.out_pos..me.payload_len];
                    let n = remaining.len().min(buf.remaining());
                    buf.put_slice(&remaining[..n]);
                    me.out_pos += n;
                    return Poll::Ready(Ok(()));
                }
                ReadState::ReadingHeader => {
                    while me.header_filled < HEADER_SIZE {
                        let mut tmp = ReadBuf::new(&mut me.header[me.header_filled..]);
                        match Pin::new(&mut me.inner).poll_read(cx, &mut tmp) {
                            Poll::Ready(Ok(())) => {
                                let n = tmp.filled().len();
                                if n == 0 {
                                    return Poll::Ready(Ok(()));
                                }
                                me.header_filled += n;
                            }
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                            Poll::Pending => return Poll::Pending,
                        }
                    }
                    let len = u32::from_be_bytes([
                        me.header[0],
                        me.header[1],
                        me.header[2],
                        me.header[3],
                    ]) as usize;
                    if len > MAX_FRAME {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("frame too large: {len} bytes"),
                        )));
                    }
                    me.payload_len = len;
                    me.payload.resize(len, 0);
                    me.payload_filled = 0;
                    me.state = ReadState::ReadingPayload;
                }
                ReadState::ReadingPayload => {
                    while me.payload_filled < me.payload_len {
                        let mut tmp =
                            ReadBuf::new(&mut me.payload[me.payload_filled..me.payload_len]);
                        match Pin::new(&mut me.inner).poll_read(cx, &mut tmp) {
                            Poll::Ready(Ok(())) => {
                                let n = tmp.filled().len();
                                if n == 0 {
                                    return Poll::Ready(Err(io::Error::new(
                                        io::ErrorKind::UnexpectedEof,
                                        "connection closed mid-frame",
                                    )));
                                }
                                me.payload_filled += n;
                            }
                            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                            Poll::Pending => return Poll::Pending,
                        }
                    }
                    let expected = &me.header[LEN_SIZE..HEADER_SIZE];
                    let computed = compute_hmac(&me.key, &me.payload[..me.payload_len]);
                    if computed.as_slice() != expected {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "HMAC verification failed",
                        )));
                    }
                    me.out_pos = 0;
                    me.state = ReadState::Yielding;
                }
            }
        }
    }
}

// ── SignedWriter ─────────────────────────────────────────────────────────

pub struct SignedWriter<W> {
    inner: W,
    key: [u8; 32],
    buf: Vec<u8>,
    frame: Vec<u8>,
    frame_pos: usize,
    flushing: bool,
}

impl<W> SignedWriter<W> {
    fn new(inner: W, key: [u8; 32]) -> Self {
        Self {
            inner,
            key,
            buf: Vec::new(),
            frame: Vec::new(),
            frame_pos: 0,
            flushing: false,
        }
    }
}

impl<W: AsyncWrite + Unpin> SignedWriter<W> {
    fn poll_flush_frame(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.frame_pos < self.frame.len() {
            match Pin::new(&mut self.inner).poll_write(cx, &self.frame[self.frame_pos..]) {
                Poll::Ready(Ok(0)) => {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write returned 0",
                    )));
                }
                Poll::Ready(Ok(n)) => self.frame_pos += n,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        self.flushing = false;
        self.frame.clear();
        Pin::new(&mut self.inner).poll_flush(cx)
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for SignedWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if me.flushing {
            match me.poll_flush_frame(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        me.buf.extend_from_slice(data);
        Poll::Ready(Ok(data.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.flushing {
            return me.poll_flush_frame(cx);
        }
        if me.buf.is_empty() {
            return Pin::new(&mut me.inner).poll_flush(cx);
        }
        let payload = std::mem::take(&mut me.buf);
        let len = payload.len() as u32;
        let hmac = compute_hmac(&me.key, &payload);

        me.frame.clear();
        me.frame.reserve(HEADER_SIZE + payload.len());
        me.frame.extend_from_slice(&len.to_be_bytes());
        me.frame.extend_from_slice(&hmac);
        me.frame.extend_from_slice(&payload);
        me.frame_pos = 0;
        me.flushing = true;

        me.poll_flush_frame(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.flushing {
            match me.poll_flush_frame(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        if !me.buf.is_empty() {
            match Pin::new(&mut *me).poll_flush(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        }
        Pin::new(&mut me.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn signed_round_trip() {
        let key = derive_key("test-token");
        let (client, server) = tokio::io::duplex(8192);
        let (cr, cw) = tokio::io::split(client);
        let (sr, sw) = tokio::io::split(server);

        let mut writer = SignedWriter::new(cw, key);
        let mut reader = SignedReader::new(sr, key);

        let data = b"hello signed world";
        writer.write_all(data).await.unwrap();
        writer.flush().await.unwrap();

        let mut buf = vec![0u8; data.len()];
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, data);

        drop(writer);
        drop(cr);
        drop(sw);
    }

    #[tokio::test]
    async fn wrong_key_rejected() {
        let key_a = derive_key("token-a");
        let key_b = derive_key("token-b");
        let (client, server) = tokio::io::duplex(8192);
        let (_cr, cw) = tokio::io::split(client);
        let (sr, _sw) = tokio::io::split(server);

        let mut writer = SignedWriter::new(cw, key_a);
        let mut reader = SignedReader::new(sr, key_b);

        writer.write_all(b"tampered").await.unwrap();
        writer.flush().await.unwrap();

        let mut buf = vec![0u8; 8];
        let result = reader.read_exact(&mut buf).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn handshake_success() {
        let key = derive_key("my-token");
        let (client, server) = tokio::io::duplex(1024);
        let (mut cr, mut cw) = tokio::io::split(client);
        let (mut sr, mut sw) = tokio::io::split(server);

        let server_task =
            tokio::spawn(async move { server_handshake(&key, &mut sr, &mut sw).await });
        // Client side: read nonce, compute HMAC, send response
        let mut nonce = [0u8; NONCE_LEN];
        cr.read_exact(&mut nonce).await.unwrap();
        let response = compute_hmac(&key, &nonce);
        cw.write_all(&response).await.unwrap();
        cw.flush().await.unwrap();

        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn handshake_wrong_token_fails() {
        let server_key = derive_key("correct");
        let client_key = derive_key("wrong");
        let (client, server) = tokio::io::duplex(1024);
        let (mut cr, mut cw) = tokio::io::split(client);
        let (mut sr, mut sw) = tokio::io::split(server);

        let server_task =
            tokio::spawn(async move { server_handshake(&server_key, &mut sr, &mut sw).await });
        let mut nonce = [0u8; NONCE_LEN];
        cr.read_exact(&mut nonce).await.unwrap();
        let response = compute_hmac(&client_key, &nonce);
        cw.write_all(&response).await.unwrap();
        cw.flush().await.unwrap();

        let result = server_task.await.unwrap();
        assert!(result.is_err());
    }
}
