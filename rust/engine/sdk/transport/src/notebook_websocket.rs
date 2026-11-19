// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded RFC 6455 JSON data path on a caller's already-upgraded byte stream.
//!
//! No HTTP handshake, TLS, authentication, discovery or readiness is performed.
//! The caller attests the immutable owner binding, selected JSON subprotocol and
//! ready Spark session. Preread bytes must be retained inside the caller's I/O.
//! Only the existing caller Tokio runtime (with time enabled) is used.

use crate::TokioTimer;
use fabric_sdk::{
    notebook_runtime::driver::{DeadlineAlarm, NotebookChannel, ReadySession},
    runtime::{NativeDeadline, NativeError, NativeFuture},
};
use futures_util::{SinkExt, StreamExt};
use std::{
    fmt,
    future::{Future, poll_fn},
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error as WebSocketError, Message,
        error::ProtocolError,
        protocol::{Role, WebSocketConfig},
    },
};

const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CONTROLS_PER_RECEIVE: usize = 1024;
const MAX_CONTROLS_TOTAL: usize = 1_000_000;
const MASKED_HEADER_BYTES: usize = 14;
const CONTROL_FRAME_BYTES: usize = 125 + MASKED_HEADER_BYTES;
const IO_QUANTUM: usize = 16;

/// Finite codec and control-traffic bounds, validated before codec allocation.
///
/// These bound protocol payloads/buffers, not RSS, caller I/O buffering, physical
/// TCP read-ahead, or allocations in the remote application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotebookWebSocketOptions {
    /// Eager codec read-buffer size, in `1..=65536` bytes.
    pub read_buffer_bytes: usize,
    /// Maximum complete message payload, in `1..=16777216` UTF-8 bytes.
    pub message_bytes: usize,
    /// Maximum incoming frame payload, positive and at most `message_bytes`.
    ///
    /// This also limits control-frame payloads. Fragmented data is reassembled
    /// by tungstenite, subject independently to the complete message limit.
    pub frame_bytes: usize,
    /// Write batching threshold, from zero through `message_bytes`.
    ///
    /// Every operation flushes. The finite maximum write buffer is derived with
    /// checked arithmetic: threshold + message + masked header + two maximum
    /// control frames (automatic Pong/Close).
    pub write_buffer_bytes: usize,
    /// Maximum Ping/Pong frames consumed by any single receive, in `1..=1024`.
    pub controls_per_receive: usize,
    /// Maximum Ping/Pong frames across this channel's lifetime, in `1..=1000000`.
    ///
    /// Must be at least `controls_per_receive`. Exceeding either cap terminates
    /// the channel, including when a peer's control stream is always ready.
    pub controls_total: usize,
}

impl Default for NotebookWebSocketOptions {
    fn default() -> Self {
        Self {
            read_buffer_bytes: 4096,
            message_bytes: 4 * 1024 * 1024,
            frame_bytes: 4 * 1024 * 1024,
            write_buffer_bytes: 0,
            controls_per_receive: 32,
            controls_total: 1024,
        }
    }
}

impl NotebookWebSocketOptions {
    fn config(self) -> Result<WebSocketConfig, NativeError> {
        if !(1..=MAX_READ_BUFFER_BYTES).contains(&self.read_buffer_bytes)
            || !(1..=MAX_MESSAGE_BYTES).contains(&self.message_bytes)
            || self.frame_bytes == 0
            || self.frame_bytes > self.message_bytes
            || self.write_buffer_bytes > self.message_bytes
            || !(1..=MAX_CONTROLS_PER_RECEIVE).contains(&self.controls_per_receive)
            || !(self.controls_per_receive..=MAX_CONTROLS_TOTAL).contains(&self.controls_total)
        {
            return Err(NativeError::InvalidInput);
        }
        let max_write = self
            .write_buffer_bytes
            .checked_add(self.message_bytes)
            .and_then(|bytes| bytes.checked_add(MASKED_HEADER_BYTES))
            .and_then(|bytes| bytes.checked_add(2 * CONTROL_FRAME_BYTES))
            .ok_or(NativeError::InvalidInput)?;
        Ok(WebSocketConfig::default()
            .read_buffer_size(self.read_buffer_bytes)
            .write_buffer_size(self.write_buffer_bytes)
            .max_write_buffer_size(max_write)
            .max_message_size(Some(self.message_bytes))
            .max_frame_size(Some(self.frame_bytes))
            .accept_unmasked_frames(false))
    }
}

/// Sanitized local lifecycle, independent of the kernel's execution outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotebookWebSocketState {
    /// Available for one caller-driven send or receive.
    Open,
    /// A send was polled and may have partially or completely reached the peer.
    SendUnknown,
    /// A failed or dropped polled operation permanently poisoned continuation.
    Failed,
    /// The socket was locally disposed; no remote cleanup is implied.
    Closed,
}

/// A real masked-client WebSocket channel on consumed, already-upgraded I/O.
///
/// There are no background tasks, prefetch, reconnects or retries. A successful
/// receive returns one complete UTF-8 text message without reparsing its JSON;
/// the SDK driver owns Jupyter/Synapse semantics. Binary messages are terminal
/// [`NativeError::UnsupportedCapability`].
///
/// Each polled operation removes the socket before its first await. Dropping
/// that future drops the consumed I/O and leaves the channel unusable. Dropping
/// an unpolled operation does not alter it. Neither drop nor Close cancels,
/// interrupts, deletes or restarts remote compute.
pub struct NotebookWebSocket<S> {
    socket: Option<WebSocketStream<DeadlineIo<S>>>,
    ready: ReadySession,
    options: NotebookWebSocketOptions,
    deadline: NativeDeadline,
    state: NotebookWebSocketState,
    controls: usize,
}

impl<S> fmt::Debug for NotebookWebSocket<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NotebookWebSocket")
            .field("state", &self.state)
            .field("controls", &self.controls)
            .finish_non_exhaustive()
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> NotebookWebSocket<S> {
    /// Consume an already HTTP-upgraded caller-owned stream, without a handshake.
    ///
    /// The caller is responsible for authenticated upgrade, correct subprotocol,
    /// any preread bytes inside `stream`, owner/session/comm and Spark readiness.
    /// `deadline` is the original lifetime budget; later deadlines can shorten
    /// but never extend it. Validation precedes codec/read-buffer allocation.
    ///
    /// # Errors
    /// Rejects invalid finite options or an expired original deadline. The
    /// consumed stream is dropped even if construction fails or is abandoned.
    pub async fn from_upgraded(
        stream: S,
        ready: ReadySession,
        options: NotebookWebSocketOptions,
        deadline: NativeDeadline,
    ) -> Result<Self, NativeError> {
        let config = options.config()?;
        deadline.remaining()?;
        let io = DeadlineIo {
            inner: stream,
            original: deadline.clone(),
            call: deadline.clone(),
            reads: 0,
            writes: 0,
        };
        let socket = WebSocketStream::from_raw_socket(io, Role::Client, Some(config)).await;
        deadline.remaining()?;
        Ok(Self {
            socket: Some(socket),
            ready,
            options,
            deadline,
            state: NotebookWebSocketState::Open,
            controls: 0,
        })
    }

    /// Current sanitized local state, not evidence about remote execution.
    pub const fn state(&self) -> NotebookWebSocketState {
        self.state
    }

    /// Total Ping/Pong frames processed, including the frame that exceeded a cap.
    pub const fn controls_received(&self) -> usize {
        self.controls
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> NotebookChannel for NotebookWebSocket<S> {
    fn binding(&self) -> &ReadySession {
        &self.ready
    }

    fn send<'a>(
        &'a mut self,
        frame: &'a [u8],
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>> {
        Box::pin(async move {
            let Some(mut socket) = self.socket.take() else {
                return Err(NativeError::OutcomeUnknown);
            };
            self.state = NotebookWebSocketState::Failed;
            check_deadlines(&self.deadline, deadline)?;
            if frame.len() > self.options.message_bytes {
                return Err(NativeError::Limit);
            }
            let text = std::str::from_utf8(frame).map_err(|_| NativeError::InvalidInput)?;
            socket.get_mut().call = deadline.clone();
            self.state = NotebookWebSocketState::SendUnknown;
            let result = until(
                &self.deadline,
                deadline,
                socket.send(Message::Text(text.to_owned().into())),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                return Err(NativeError::OutcomeUnknown);
            }
            self.socket = Some(socket);
            self.state = NotebookWebSocketState::Open;
            Ok(())
        })
    }

    fn receive<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            let Some(mut socket) = self.socket.take() else {
                return if self.state == NotebookWebSocketState::Closed {
                    Ok(None)
                } else {
                    Err(NativeError::Transport)
                };
            };
            self.state = NotebookWebSocketState::Failed;
            check_deadlines(&self.deadline, deadline)?;
            // 0.30 has no public dynamic config setter. Reject before I/O rather
            // than allocating to the larger codec cap and checking afterwards.
            if max_bytes < self.options.message_bytes {
                return Err(NativeError::UnsupportedCapability);
            }
            socket.get_mut().call = deadline.clone();
            let mut controls = 0;
            loop {
                let message = until(&self.deadline, deadline, socket.next())
                    .await?
                    .transpose()
                    .map_err(receive_error)?;
                match message {
                    Some(Message::Text(text)) => {
                        let frame = text.as_bytes().to_vec();
                        check_deadlines(&self.deadline, deadline)?;
                        self.socket = Some(socket);
                        self.state = NotebookWebSocketState::Open;
                        return Ok(Some(frame));
                    }
                    Some(Message::Binary(_)) => {
                        return Err(NativeError::UnsupportedCapability);
                    }
                    Some(Message::Ping(_)) | Some(Message::Pong(_)) => {
                        controls += 1;
                        self.controls += 1;
                        check_deadlines(&self.deadline, deadline)?;
                        if controls > self.options.controls_per_receive
                            || self.controls > self.options.controls_total
                        {
                            return Err(NativeError::Limit);
                        }
                        // tungstenite queues an automatic Pong on Ping; flush
                        // it unchanged, never replace it with an invented Pong.
                        until(&self.deadline, deadline, socket.flush())
                            .await?
                            .map_err(receive_error)?;
                    }
                    Some(Message::Close(_)) => {
                        // Flush tungstenite's automatic close response and then
                        // dispose locally, without waiting for kernel cleanup.
                        until(&self.deadline, deadline, socket.flush())
                            .await?
                            .map_err(receive_error)?;
                        self.state = NotebookWebSocketState::Closed;
                        return Ok(None);
                    }
                    None => {
                        self.state = NotebookWebSocketState::Closed;
                        return Ok(None);
                    }
                    Some(Message::Frame(_)) => return Err(NativeError::InvalidResponse),
                }
            }
        })
    }

    fn close<'a>(
        &'a mut self,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>> {
        Box::pin(async move {
            let socket = self.socket.take();
            self.state = NotebookWebSocketState::Closed;
            let Some(mut socket) = socket else {
                return Ok(());
            };
            check_deadlines(&self.deadline, deadline)?;
            socket.get_mut().call = deadline.clone();
            // Best-effort RFC Close + flush, not a remote acknowledgement,
            // rollback or cancellation. Always drop the socket on completion.
            until(&self.deadline, deadline, socket.close(None))
                .await?
                .map_err(receive_error)
        })
    }
}

impl DeadlineAlarm for TokioTimer {
    fn wait<'a>(&'a self, deadline: &'a NativeDeadline) -> NativeFuture<'a, ()> {
        Box::pin(async move {
            while let Ok(remaining) = deadline.remaining() {
                tokio::time::sleep(remaining).await;
            }
        })
    }
}

fn check_deadlines(original: &NativeDeadline, call: &NativeDeadline) -> Result<(), NativeError> {
    original.remaining()?;
    call.remaining()?;
    Ok(())
}

async fn until<F: Future>(
    original: &NativeDeadline,
    call: &NativeDeadline,
    future: F,
) -> Result<F::Output, NativeError> {
    check_deadlines(original, call)?;
    let timer = TokioTimer;
    let mut original_alarm = timer.wait(original);
    let mut call_alarm = timer.wait(call);
    let mut future = std::pin::pin!(future);
    poll_fn(|cx| {
        if let Err(error) = check_deadlines(original, call) {
            return Poll::Ready(Err(error));
        }
        if original_alarm.as_mut().poll(cx).is_ready() || call_alarm.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(NativeError::TimedOut));
        }
        let result = future.as_mut().poll(cx);
        if let Err(error) = check_deadlines(original, call) {
            return Poll::Ready(Err(error));
        }
        result.map(Ok)
    })
    .await
}

fn receive_error(error: WebSocketError) -> NativeError {
    match error {
        WebSocketError::Capacity(_) | WebSocketError::WriteBufferFull(_) => NativeError::Limit,
        WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
        | WebSocketError::Io(_) => NativeError::Transport,
        WebSocketError::Protocol(_) | WebSocketError::Utf8(_) => NativeError::InvalidResponse,
        _ => NativeError::Transport,
    }
}

// The codec can consume many ready continuation frames inside one Stream poll.
// Check the absolute clock at the byte boundary and yield after finite I/O work,
// including for a peer continuously sending empty nonfinal fragments.
struct DeadlineIo<S> {
    inner: S,
    original: NativeDeadline,
    call: NativeDeadline,
    reads: usize,
    writes: usize,
}

impl<S> DeadlineIo<S> {
    fn check(&self) -> io::Result<()> {
        check_deadlines(&self.original, &self.call)
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))
    }
}

fn quantum(counter: &mut usize, cx: &Context<'_>) -> bool {
    if *counter == IO_QUANTUM {
        *counter = 0;
        cx.waker().wake_by_ref();
        false
    } else {
        *counter += 1;
        true
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for DeadlineIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check()?;
        if !quantum(&mut this.reads, cx) {
            return Poll::Pending;
        }
        let result = Pin::new(&mut this.inner).poll_read(cx, buffer);
        this.check()?;
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for DeadlineIo<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check()?;
        if !quantum(&mut this.writes, cx) {
            return Poll::Pending;
        }
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        this.check()?;
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check()?;
        let result = Pin::new(&mut this.inner).poll_flush(cx);
        this.check()?;
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check()?;
        let result = Pin::new(&mut this.inner).poll_shutdown(cx);
        this.check()?;
        result
    }
}
