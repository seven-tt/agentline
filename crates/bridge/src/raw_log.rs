//! Transparent line-logging wrappers around an agent's stdio.
//!
//! Emits every raw ACP JSON-RPC line at `trace!` (target `acp::raw`) tagged with
//! the agent name and direction (`←agent` = read from agent, `→agent` = written
//! to agent). This is the per-agent "raw ACP input/output" required for
//! debugging.
//!
//! **Zero cost in release builds**: with the `release_max_level_info` tracing
//! feature the `trace!` (and the `enabled!` guard) compile out entirely, so the
//! buffering below never runs in a release package.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures::io::{AsyncRead, AsyncWrite};

/// Wraps an async byte stream and logs each complete newline-delimited line that
/// passes through, without altering the bytes.
pub struct LineLog<S> {
    inner: S,
    agent: String,
    dir: &'static str,
    buf: Vec<u8>,
}

impl<S> LineLog<S> {
    /// Wrap the agent→client (read) side.
    pub fn reader(inner: S, agent: String) -> Self {
        Self {
            inner,
            agent,
            dir: "←agent",
            buf: Vec::new(),
        }
    }

    /// Wrap the client→agent (write) side.
    pub fn writer(inner: S, agent: String) -> Self {
        Self {
            inner,
            agent,
            dir: "→agent",
            buf: Vec::new(),
        }
    }

    fn log(&mut self, bytes: &[u8]) {
        // Cheap guard: compiled out in release (max level capped at info), and a
        // no-op at runtime unless `acp::raw` is enabled at TRACE.
        if !tracing::enabled!(target: "acp::raw", tracing::Level::TRACE) {
            return;
        }
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end();
            if !text.is_empty() {
                tracing::trace!(target: "acp::raw", agent = %self.agent, dir = self.dir, "{text}");
            }
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for LineLog<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let n = match Pin::new(&mut this.inner).poll_read(cx, out) {
            Poll::Ready(Ok(n)) => n,
            other => return other,
        };
        if n > 0 {
            let copy = out[..n].to_vec();
            this.log(&copy);
        }
        Poll::Ready(Ok(n))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for LineLog<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let n = match Pin::new(&mut this.inner).poll_write(cx, data) {
            Poll::Ready(Ok(n)) => n,
            other => return other,
        };
        if n > 0 {
            let copy = data[..n].to_vec();
            this.log(&copy);
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}
