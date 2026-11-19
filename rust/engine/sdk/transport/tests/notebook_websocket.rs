// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline RFC 6455 peers and actual managed-driver composition; no HTTP/auth.
#![cfg(feature = "notebook-websocket")]
#![deny(unnameable_test_items)]

use fabric_sdk::{
    notebook_runtime::{
        CommId, ExecutionId, KernelId, MessageId, NotebookRuntime, ResumeBinding, SessionId,
        content::ContentLimits,
        driver::{
            DriverEvent, DriverLimits, DriverOptions, DriverState, ManagedExecution,
            NotebookChannel, OutputSink, ReadySession, RemoteOutcome,
        },
        protocol::{
            ExecuteRequest, ExecuteStatus, JupyterMessage, LiveOutput, MessageHeader,
            ProtocolLimits,
        },
    },
    runtime::{NativeDeadline, NativeEndpoint, NativeError, NativeFuture},
};
use fabric_sdk_transport::{
    TokioTimer,
    notebook_websocket::{NotebookWebSocket, NotebookWebSocketOptions, NotebookWebSocketState},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{
            Role, WebSocketConfig,
            frame::{
                Frame,
                coding::{Data, OpCode},
            },
        },
    },
};

#[derive(Default)]
struct Probe {
    reads: AtomicUsize,
    writes: AtomicUsize,
    read_bytes: AtomicUsize,
    drops: AtomicUsize,
    stall_read: AtomicBool,
    stall_write: AtomicBool,
    stall_flush: AtomicBool,
    reset_read: AtomicBool,
    late_read: AtomicBool,
    written: Mutex<Vec<u8>>,
}

struct ProbeIo {
    inner: DuplexStream,
    probe: Arc<Probe>,
}

impl fmt::Debug for ProbeIo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sensitive-io-url-owner-payload")
    }
}

impl Drop for ProbeIo {
    fn drop(&mut self) {
        self.probe.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncRead for ProbeIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.probe.reads.fetch_add(1, Ordering::SeqCst);
        if self.probe.reset_read.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "sensitive-reset-url-owner-payload",
            )));
        }
        if self.probe.stall_read.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        if self.probe.late_read.swap(false, Ordering::SeqCst) {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(100) {
                std::hint::spin_loop();
            }
        }
        let mut bytes = [0; 128];
        let capacity = buffer.remaining().min(bytes.len());
        let mut read = ReadBuf::new(&mut bytes[..capacity]);
        let result = Pin::new(&mut self.inner).poll_read(cx, &mut read);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.probe
                .read_bytes
                .fetch_add(read.filled().len(), Ordering::SeqCst);
            buffer.put_slice(read.filled());
        }
        result
    }
}

impl AsyncWrite for ProbeIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.probe.writes.fetch_add(1, Ordering::SeqCst);
        if self.probe.stall_write.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        let result = Pin::new(&mut self.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(length)) = result {
            self.probe
                .written
                .lock()
                .unwrap()
                .extend_from_slice(&bytes[..length]);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.probe.stall_flush.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn pair(capacity: usize) -> (ProbeIo, DuplexStream, Arc<Probe>) {
    let (client, server) = tokio::io::duplex(capacity);
    let probe = Arc::new(Probe::default());
    (
        ProbeIo {
            inner: client,
            probe: Arc::clone(&probe),
        },
        server,
        probe,
    )
}

fn ready_for(notebook: &str) -> ReadySession {
    let runtime = NotebookRuntime::new(
        NativeEndpoint::new("https://sensitive-origin.invalid".parse().unwrap()).unwrap(),
        "sensitive-capacity",
        "sensitive-workspace",
        notebook,
        ContentLimits::default(),
        1024,
        10,
    )
    .unwrap();
    ReadySession::new(
        &runtime,
        ResumeBinding {
            session: SessionId::new("sensitive-session").unwrap(),
            kernel: KernelId::new("sensitive-kernel").unwrap(),
        },
        CommId::new("sensitive-control").unwrap(),
    )
}

fn ready() -> ReadySession {
    ready_for("sensitive-notebook")
}

fn deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_secs(10)).unwrap()
}

fn short_deadline() -> NativeDeadline {
    NativeDeadline::new(Duration::from_millis(100)).unwrap()
}

fn options(cap: usize) -> NotebookWebSocketOptions {
    NotebookWebSocketOptions {
        read_buffer_bytes: 32,
        message_bytes: cap,
        frame_bytes: cap,
        ..NotebookWebSocketOptions::default()
    }
}

async fn make_channel(
    io: ProbeIo,
    options: NotebookWebSocketOptions,
    deadline: NativeDeadline,
) -> NotebookWebSocket<ProbeIo> {
    NotebookWebSocket::from_upgraded(io, ready(), options, deadline)
        .await
        .unwrap()
}

async fn peer(io: DuplexStream) -> WebSocketStream<DuplexStream> {
    WebSocketStream::from_raw_socket(
        io,
        Role::Server,
        Some(
            WebSocketConfig::default()
                .read_buffer_size(128)
                .write_buffer_size(0)
                .max_write_buffer_size(8192)
                .max_message_size(Some(4096))
                .max_frame_size(Some(4096)),
        ),
    )
    .await
}

fn assert_pending<F: Future + ?Sized>(future: Pin<&mut F>) {
    let mut context = Context::from_waker(Waker::noop());
    assert!(future.poll(&mut context).is_pending());
}

fn no_io(probe: &Probe) {
    assert_eq!(probe.reads.load(Ordering::SeqCst), 0);
    assert_eq!(probe.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sends_real_masked_client_text_and_counts_unicode_bytes() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(4), deadline()).await;
    no_io(&probe);
    let mut server = peer(server).await;
    let payload = "\"é\"";
    assert_eq!(payload.len(), 4);
    channel.send(payload.as_bytes(), &deadline()).await.unwrap();
    assert_eq!(
        server.next().await.unwrap().unwrap(),
        Message::text(payload)
    );
    let bytes = probe.written.lock().unwrap();
    assert_eq!(bytes[0], 0x81);
    assert_eq!(bytes[1] & 0x80, 0x80);
    assert_eq!(usize::from(bytes[1] & 0x7f), payload.len());
    assert_eq!(bytes.len(), payload.len() + 6);
    for (index, byte) in payload.bytes().enumerate() {
        assert_eq!(bytes[6 + index] ^ bytes[2 + index % 4], byte);
    }
}

#[tokio::test]
async fn receives_one_text_message_and_reassembles_fragmented_unicode() {
    let (io, server, _) = pair(2048);
    let mut channel = make_channel(io, options(64), deadline()).await;
    let mut server = peer(server).await;
    server.send(Message::text("{\"a\":1}")).await.unwrap();
    server
        .send(Message::Frame(Frame::message(
            vec![b'"', 0xc3],
            OpCode::Data(Data::Text),
            false,
        )))
        .await
        .unwrap();
    server
        .send(Message::Frame(Frame::message(
            vec![0xa9, b'"'],
            OpCode::Data(Data::Continue),
            true,
        )))
        .await
        .unwrap();
    assert_eq!(
        channel.receive(64, &deadline()).await.unwrap(),
        Some(b"{\"a\":1}".to_vec())
    );
    assert_eq!(
        channel.receive(64, &deadline()).await.unwrap(),
        Some("\"é\"".as_bytes().to_vec())
    );
    // Transport deliberately leaves JSON/Jupyter semantic validation to the SDK.
    server.send(Message::text("not JSON")).await.unwrap();
    assert_eq!(
        channel.receive(64, &deadline()).await.unwrap(),
        Some(b"not JSON".to_vec())
    );
}

#[tokio::test]
async fn rejects_declared_oversize_frame_or_message_before_body_poll() {
    for (opts, header) in [
        (options(32), vec![0x81, 33]),
        (
            NotebookWebSocketOptions {
                frame_bytes: 16,
                ..options(32)
            },
            vec![0x81, 17],
        ),
        (
            options(32),
            vec![0x81, 127, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ),
    ] {
        let (io, mut server, probe) = pair(1024);
        let mut channel = make_channel(io, opts, deadline()).await;
        server.write_all(&header).await.unwrap();
        assert_eq!(
            channel.receive(32, &deadline()).await,
            Err(NativeError::Limit)
        );
        assert_eq!(probe.reads.load(Ordering::SeqCst), 1);
        assert_eq!(probe.read_bytes.load(Ordering::SeqCst), header.len());
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn fragmented_message_sum_is_independently_bounded() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(32), deadline()).await;
    let mut server = peer(server).await;
    for (opcode, final_frame) in [(Data::Text, false), (Data::Continue, true)] {
        server
            .send(Message::Frame(Frame::message(
                vec![b'x'; 20],
                OpCode::Data(opcode),
                final_frame,
            )))
            .await
            .unwrap();
    }
    assert_eq!(
        channel.receive(32, &deadline()).await,
        Err(NativeError::Limit)
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn smaller_per_call_budget_is_terminal_and_rejected_before_io() {
    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(32), deadline()).await;
    assert_eq!(
        channel.receive(31, &deadline()).await,
        Err(NativeError::UnsupportedCapability)
    );
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(channel.state(), NotebookWebSocketState::Failed);
    assert!(channel.send(b"{}", &deadline()).await.is_err());
    no_io(&probe);
}

#[tokio::test]
async fn invalid_options_reject_before_codec_or_io() {
    let defaults = NotebookWebSocketOptions::default();
    for invalid in [
        NotebookWebSocketOptions {
            read_buffer_bytes: 0,
            ..defaults
        },
        NotebookWebSocketOptions {
            read_buffer_bytes: 65537,
            ..defaults
        },
        NotebookWebSocketOptions {
            message_bytes: 0,
            ..defaults
        },
        NotebookWebSocketOptions {
            message_bytes: usize::MAX,
            ..defaults
        },
        NotebookWebSocketOptions {
            frame_bytes: 0,
            ..defaults
        },
        NotebookWebSocketOptions {
            frame_bytes: defaults.message_bytes + 1,
            ..defaults
        },
        NotebookWebSocketOptions {
            write_buffer_bytes: usize::MAX,
            ..defaults
        },
        NotebookWebSocketOptions {
            controls_per_receive: 0,
            ..defaults
        },
        NotebookWebSocketOptions {
            controls_per_receive: 1025,
            ..defaults
        },
        NotebookWebSocketOptions {
            controls_total: 0,
            ..defaults
        },
        NotebookWebSocketOptions {
            controls_total: usize::MAX,
            ..defaults
        },
    ] {
        let (io, _server, probe) = pair(1024);
        assert_eq!(
            NotebookWebSocket::from_upgraded(io, ready(), invalid, deadline())
                .await
                .unwrap_err(),
            NativeError::InvalidInput
        );
        no_io(&probe);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn send_preflight_checks_bytes_and_utf8_without_copying_to_io() {
    for (payload, error) in [
        ("\"éé\"".as_bytes(), NativeError::Limit),
        (&[0xff][..], NativeError::InvalidInput),
    ] {
        let (io, _server, probe) = pair(1024);
        let mut channel = make_channel(io, options(4), deadline()).await;
        assert_eq!(channel.send(payload, &deadline()).await, Err(error));
        no_io(&probe);
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_eq!(channel.state(), NotebookWebSocketState::Failed);
    }
}

#[tokio::test]
async fn binary_is_terminal_unsupported_not_json_even_when_bytes_are_json() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(64), deadline()).await;
    let mut server = peer(server).await;
    server.send(Message::binary(b"{}".to_vec())).await.unwrap();
    assert_eq!(
        channel.receive(64, &deadline()).await,
        Err(NativeError::UnsupportedCapability)
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        channel.receive(64, &deadline()).await,
        Err(NativeError::Transport)
    );
}

#[tokio::test]
async fn malformed_utf8_protocol_and_masked_server_frames_are_redacted() {
    for wire in [
        vec![0x81, 1, 0xff],
        vec![0xc1, 0],
        vec![0x81, 0x80, 1, 2, 3, 4],
    ] {
        let (io, mut server, probe) = pair(1024);
        let mut channel = make_channel(io, options(64), deadline()).await;
        server.write_all(&wire).await.unwrap();
        let error = channel.receive(64, &deadline()).await.unwrap_err();
        assert_eq!(error, NativeError::InvalidResponse);
        assert!(!format!("{error:?} {error}").contains("sensitive"));
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn automatic_pong_is_flushed_unchanged_under_the_receive_deadline() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    let mut server = peer(server).await;
    let ping = vec![b'p'; 125];
    server
        .send(Message::Ping(ping.clone().into()))
        .await
        .unwrap();
    server.send(Message::text("{}")).await.unwrap();
    assert_eq!(
        channel.receive(256, &deadline()).await.unwrap(),
        Some(b"{}".to_vec())
    );
    assert_eq!(
        server.next().await.unwrap().unwrap(),
        Message::Pong(ping.into())
    );
    assert_eq!(channel.controls_received(), 1);
    assert!(probe.writes.load(Ordering::SeqCst) > 0);
}

#[tokio::test]
async fn ping_flush_stall_times_out_and_drops_the_socket() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), short_deadline()).await;
    let mut server = peer(server).await;
    server.send(Message::Ping(vec![1].into())).await.unwrap();
    probe.stall_write.store(true, Ordering::SeqCst);
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::TimedOut)
    );
    assert_eq!(channel.controls_received(), 1);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ready_control_flood_has_per_receive_and_lifetime_limits() {
    let opts = NotebookWebSocketOptions {
        controls_per_receive: 2,
        controls_total: 3,
        ..options(256)
    };
    let (io, server, probe) = pair(4096);
    let mut channel = make_channel(io, opts, deadline()).await;
    let mut server = peer(server).await;
    for _ in 0..2 {
        server.send(Message::Pong(vec![1].into())).await.unwrap();
    }
    server.send(Message::text("{}")).await.unwrap();
    assert!(channel.receive(256, &deadline()).await.unwrap().is_some());
    for _ in 0..2 {
        server.send(Message::Pong(vec![2].into())).await.unwrap();
    }
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::Limit)
    );
    assert_eq!(channel.controls_received(), 4);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

    let (io, server, _) = pair(4096);
    let mut channel = make_channel(io, opts, deadline()).await;
    let mut server = peer(server).await;
    for _ in 0..3 {
        server.send(Message::Ping(vec![3].into())).await.unwrap();
    }
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::Limit)
    );
    assert_eq!(channel.controls_received(), 3);
}

#[tokio::test]
async fn genuine_close_flushes_response_but_raw_eof_and_reset_are_transport_errors() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    let mut server = peer(server).await;
    server.send(Message::Close(None)).await.unwrap();
    assert_eq!(channel.receive(256, &deadline()).await.unwrap(), None);
    assert!(matches!(
        server.next().await,
        Some(Ok(Message::Close(None)))
    ));
    assert_eq!(channel.state(), NotebookWebSocketState::Closed);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(channel.receive(256, &deadline()).await.unwrap(), None);

    for reset in [false, true] {
        let (io, server, probe) = pair(1024);
        let mut channel = make_channel(io, options(256), deadline()).await;
        probe.reset_read.store(reset, Ordering::SeqCst);
        drop(server);
        let error = channel.receive(256, &deadline()).await.unwrap_err();
        assert_eq!(error, NativeError::Transport);
        assert!(!format!("{error:?} {error}").contains("sensitive"));
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn local_close_sends_only_rfc_close_and_disposes_without_remote_ack() {
    let (io, server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    let mut server = peer(server).await;
    channel.close(&deadline()).await.unwrap();
    assert!(matches!(
        server.next().await,
        Some(Ok(Message::Close(None)))
    ));
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    let writes = probe.writes.load(Ordering::SeqCst);
    channel.close(&deadline()).await.unwrap();
    assert_eq!(probe.writes.load(Ordering::SeqCst), writes);
}

#[tokio::test]
async fn stalled_send_receive_and_close_race_the_existing_runtime_timer() {
    let started = Instant::now();
    let (io, _server, probe) = pair(1);
    let mut channel = make_channel(io, options(256), short_deadline()).await;
    assert_eq!(
        channel.send(&[b'x'; 128], &deadline()).await,
        Err(NativeError::OutcomeUnknown)
    );
    assert_eq!(channel.state(), NotebookWebSocketState::SendUnknown);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    let writes = probe.writes.load(Ordering::SeqCst);
    assert_eq!(
        channel.send(b"{}", &deadline()).await,
        Err(NativeError::OutcomeUnknown)
    );
    assert_eq!(probe.writes.load(Ordering::SeqCst), writes);

    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), short_deadline()).await;
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::TimedOut)
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

    let (io, _server, probe) = pair(1024);
    probe.stall_flush.store(true, Ordering::SeqCst);
    let mut channel = make_channel(io, options(256), short_deadline()).await;
    assert_eq!(channel.close(&deadline()).await, Err(NativeError::TimedOut));
    assert_eq!(channel.state(), NotebookWebSocketState::Closed);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn fresh_call_deadline_does_not_extend_original_and_pauses_consume_budget() {
    let started = Instant::now();
    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), short_deadline()).await;
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::TimedOut)
    );
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(1));

    let (io, _server, probe) = pair(1024);
    let original = short_deadline();
    let mut channel = make_channel(io, options(256), original.clone()).await;
    while original.remaining().is_ok() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        channel.send(b"{}", &deadline()).await,
        Err(NativeError::TimedOut)
    );
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

    let (io, _server, _) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    assert_eq!(
        channel.receive(256, &short_deadline()).await,
        Err(NativeError::TimedOut)
    );
}

#[tokio::test]
async fn late_ready_io_is_rejected_after_poll_not_reported_as_success() {
    let (io, mut server, probe) = pair(1024);
    let mut channel = make_channel(
        io,
        options(256),
        NativeDeadline::new(Duration::from_millis(50)).unwrap(),
    )
    .await;
    server.write_all(&[0x81, 2, b'{', b'}']).await.unwrap();
    probe.late_read.store(true, Ordering::SeqCst);
    assert_eq!(
        channel.receive(256, &deadline()).await,
        Err(NativeError::TimedOut)
    );
    assert!(probe.reads.load(Ordering::SeqCst) > 0);
    assert!(!probe.late_read.load(Ordering::SeqCst));
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unpolled_operations_preserve_io_but_polled_drop_poison_disposes() {
    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    let budget = deadline();
    drop(channel.send(b"{}", &budget));
    drop(channel.receive(256, &budget));
    drop(channel.close(&budget));
    assert_eq!(channel.state(), NotebookWebSocketState::Open);
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 0);

    for operation in 0..3 {
        let (io, _server, probe) = pair(1024);
        probe.stall_read.store(true, Ordering::SeqCst);
        probe.stall_write.store(true, Ordering::SeqCst);
        let mut channel = make_channel(io, options(256), deadline()).await;
        let budget = deadline();
        match operation {
            0 => {
                let mut future = channel.send(b"{}", &budget);
                assert_pending(future.as_mut());
            }
            1 => {
                let mut future = channel.receive(256, &budget);
                assert_pending(future.as_mut());
            }
            _ => {
                let mut future = channel.close(&budget);
                assert_pending(future.as_mut());
            }
        }
        assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
        assert_ne!(channel.state(), NotebookWebSocketState::Open);
        let writes = probe.writes.load(Ordering::SeqCst);
        assert!(channel.send(b"{}", &budget).await.is_err());
        assert_eq!(probe.writes.load(Ordering::SeqCst), writes);
    }
}

#[tokio::test]
async fn constructor_future_and_channel_drop_release_consumed_io() {
    let (io, _server, probe) = pair(1024);
    drop(NotebookWebSocket::from_upgraded(
        io,
        ready(),
        options(256),
        deadline(),
    ));
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    no_io(&probe);
    let (io, _server, probe) = pair(1024);
    let channel = make_channel(io, options(256), deadline()).await;
    drop(channel);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    no_io(&probe);
}

#[tokio::test]
async fn owner_binding_is_immutable_and_debug_does_not_delegate_to_io_or_owner() {
    let (io, _server, _) = pair(1024);
    let channel = make_channel(io, options(256), deadline()).await;
    assert_eq!(channel.binding(), &ready());
    assert_ne!(channel.binding(), &ready_for("another-notebook"));
    let debug = format!("{channel:?} {:?}", channel.binding());
    for secret in ["sensitive", "https:", "session", "kernel", "payload"] {
        assert!(!debug.contains(secret), "{debug}");
    }
}

#[tokio::test]
async fn ready_empty_fragment_flood_yields_within_a_finite_io_quantum() {
    let (io, mut server, probe) = pair(8192);
    let mut channel = make_channel(io, options(256), deadline()).await;
    let mut bytes = vec![0x01, 0];
    for _ in 0..2000 {
        bytes.extend_from_slice(&[0x00, 0]);
    }
    server.write_all(&bytes).await.unwrap();
    let budget = deadline();
    let mut future = channel.receive(256, &budget);
    assert_pending(future.as_mut());
    assert!(probe.reads.load(Ordering::SeqCst) <= 16);
    drop(future);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

struct CountedChannel {
    inner: NotebookWebSocket<ProbeIo>,
    receives: Arc<AtomicUsize>,
}

impl NotebookChannel for CountedChannel {
    fn binding(&self) -> &ReadySession {
        self.inner.binding()
    }

    fn send<'a>(
        &'a mut self,
        frame: &'a [u8],
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>> {
        self.inner.send(frame, deadline)
    }

    fn receive<'a>(
        &'a mut self,
        max_bytes: usize,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<Option<Vec<u8>>, NativeError>> {
        Box::pin(async move {
            self.receives.fetch_add(1, Ordering::SeqCst);
            self.inner.receive(max_bytes, deadline).await
        })
    }

    fn close<'a>(
        &'a mut self,
        deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>> {
        self.inner.close(deadline)
    }
}

struct Sink {
    released: Arc<AtomicBool>,
    outputs: usize,
}

impl OutputSink for Sink {
    fn write<'a>(
        &'a mut self,
        index: usize,
        output: &'a LiveOutput,
        max_bytes: usize,
        _deadline: &'a NativeDeadline,
    ) -> NativeFuture<'a, Result<(), NativeError>> {
        Box::pin(async move {
            assert_eq!(index, self.outputs);
            assert_eq!(output.content["text"], json!("live-é\n"));
            assert!(max_bytes <= 2048);
            // This manually gated sink is polled explicitly by the tests below;
            // it performs no I/O and deliberately does not spawn a waker task.
            std::future::poll_fn(|_| {
                if self.released.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            self.outputs += 1;
            Ok(())
        })
    }
}

struct ManagedFixture {
    driver: ManagedExecution,
    server: WebSocketStream<DuplexStream>,
    probe: Arc<Probe>,
    receives: Arc<AtomicUsize>,
}

fn protocol_limits() -> ProtocolLimits {
    ProtocolLimits {
        frame_bytes: 2048,
        ..ProtocolLimits::default()
    }
}

fn request() -> ExecuteRequest {
    ExecuteRequest::new(
        MessageId::new("actual-execute-request").unwrap(),
        ready().session().session.clone(),
        "print('live-é')",
        false,
        true,
        protocol_limits(),
    )
    .unwrap()
}

async fn managed_fixture() -> ManagedFixture {
    let (io, server, probe) = pair(16384);
    let budget = deadline();
    let channel = make_channel(io, options(2048), budget.clone()).await;
    let receives = Arc::new(AtomicUsize::new(0));
    let driver = ManagedExecution::new(
        Box::new(CountedChannel {
            inner: channel,
            receives: Arc::clone(&receives),
        }),
        Box::new(TokioTimer),
        &request(),
        DriverOptions {
            ready: ready(),
            execution: ExecutionId::new("local-execution").unwrap(),
            limits: DriverLimits {
                protocol: protocol_limits(),
                frames: 16,
                received_bytes: 16384,
            },
            deadline: budget,
        },
    )
    .unwrap();
    ManagedFixture {
        driver,
        server: peer(server).await,
        probe,
        receives,
    }
}

async fn received_request(server: &mut WebSocketStream<DuplexStream>) -> JupyterMessage {
    let Message::Text(text) = server.next().await.unwrap().unwrap() else {
        panic!("client did not send a real text execute request");
    };
    let message = JupyterMessage::from_slice(text.as_bytes(), protocol_limits()).unwrap();
    assert_eq!(message.header.msg_type, "execute_request");
    assert_eq!(message.header.version, "5.3");
    assert_eq!(message.header.session, ready().session().session.as_str());
    assert_eq!(message.content["code"], json!("print('live-é')"));
    message
}

fn inbound(request: &JupyterMessage, id: &str, kind: &str, content: Value) -> JupyterMessage {
    JupyterMessage {
        header: MessageHeader {
            msg_id: id.into(),
            msg_type: kind.into(),
            username: "kernel".into(),
            session: "remote-kernel-protocol-session".into(),
            version: "5.3".into(),
            extra: Default::default(),
        },
        parent_header: serde_json::to_value(&request.header)
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
        metadata: Default::default(),
        content: content.as_object().unwrap().clone(),
        channel: if kind == "execute_reply" {
            "shell"
        } else {
            "iopub"
        }
        .into(),
        extra: Default::default(),
    }
}

async fn send_native(server: &mut WebSocketStream<DuplexStream>, message: JupyterMessage) {
    let bytes = message.to_vec(protocol_limits()).unwrap();
    server
        .send(Message::text(String::from_utf8(bytes).unwrap()))
        .await
        .unwrap();
}

async fn queue_output_and_reply(
    server: &mut WebSocketStream<DuplexStream>,
    request: &JupyterMessage,
    status: &str,
) {
    send_native(
        server,
        inbound(
            request,
            "out",
            "stream",
            json!({"name":"stdout","text":"live-é\n"}),
        ),
    )
    .await;
    let content = if status == "error" {
        json!({
            "status":"error", "execution_count":1,
            "ename":"SyntheticError", "evalue":"synthetic workload failure", "traceback":[]
        })
    } else {
        json!({"status":status,"execution_count":1})
    };
    send_native(
        server,
        inbound(request, "terminal", "execute_reply", content),
    )
    .await;
}

#[tokio::test]
async fn actual_managed_execution_observes_native_success_error_and_abort_over_real_peer() {
    for (status, expected) in [
        ("ok", ExecuteStatus::Ok),
        ("error", ExecuteStatus::Error),
        ("abort", ExecuteStatus::Abort),
    ] {
        let mut fixture = managed_fixture().await;
        fixture.driver.submit().await.unwrap();
        let request = received_request(&mut fixture.server).await;
        queue_output_and_reply(&mut fixture.server, &request, status).await;
        let mut sink = Sink {
            released: Arc::new(AtomicBool::new(true)),
            outputs: 0,
        };
        assert_eq!(fixture.driver.run(&mut sink).await.unwrap(), expected);
        assert_eq!(
            fixture.driver.remote_outcome(),
            RemoteOutcome::Finished(expected)
        );
        assert_eq!(fixture.driver.state(), DriverState::Finished);
        assert_eq!(fixture.driver.delivered_outputs(), 1);
        assert_eq!(fixture.driver.observer().outputs().len(), 1);
        assert_eq!(fixture.driver.observer().reply().unwrap().status, expected);
        assert_eq!(sink.outputs, 1);
        assert_eq!(fixture.receives.load(Ordering::SeqCst), 2);
        let writes = fixture.probe.writes.load(Ordering::SeqCst);
        assert!(fixture.driver.submit().await.is_err());
        assert_eq!(fixture.probe.writes.load(Ordering::SeqCst), writes);
        fixture.driver.close().await.unwrap();
        assert_eq!(fixture.probe.drops.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn managed_sink_backpressure_prevents_another_logical_receive_or_io_poll() {
    let mut fixture = managed_fixture().await;
    fixture.driver.submit().await.unwrap();
    let request = received_request(&mut fixture.server).await;
    queue_output_and_reply(&mut fixture.server, &request, "ok").await;
    let released = Arc::new(AtomicBool::new(false));
    let mut sink = Sink {
        released: Arc::clone(&released),
        outputs: 0,
    };
    let mut observing = Box::pin(fixture.driver.observe_next(&mut sink));
    assert_pending(observing.as_mut());
    assert_eq!(fixture.receives.load(Ordering::SeqCst), 1);
    let reads = fixture.probe.reads.load(Ordering::SeqCst);
    assert_pending(observing.as_mut());
    assert_eq!(fixture.receives.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.probe.reads.load(Ordering::SeqCst), reads);
    released.store(true, Ordering::SeqCst);
    assert_eq!(observing.await.unwrap(), DriverEvent::Output(0));
    assert_eq!(
        fixture.driver.observe_next(&mut sink).await.unwrap(),
        DriverEvent::Finished(ExecuteStatus::Ok)
    );
    assert_eq!(fixture.receives.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn managed_negative_correlation_cannot_finish_and_wrong_parent_session_fails_closed() {
    let mut fixture = managed_fixture().await;
    fixture.driver.submit().await.unwrap();
    let request = received_request(&mut fixture.server).await;
    let mut unrelated = inbound(
        &request,
        "unrelated",
        "execute_reply",
        json!({"status":"ok"}),
    );
    unrelated
        .parent_header
        .insert("msg_id".into(), json!("other-request"));
    send_native(&mut fixture.server, unrelated).await;
    let mut sink = Sink {
        released: Arc::new(AtomicBool::new(true)),
        outputs: 0,
    };
    assert_eq!(
        fixture.driver.observe_next(&mut sink).await.unwrap(),
        DriverEvent::Skipped
    );
    assert_eq!(fixture.driver.remote_outcome(), RemoteOutcome::Unknown);
    let mut wrong = inbound(
        &request,
        "wrong-owner",
        "execute_reply",
        json!({"status":"ok"}),
    );
    wrong
        .parent_header
        .insert("session".into(), json!("wrong-session"));
    send_native(&mut fixture.server, wrong).await;
    let error = fixture.driver.observe_next(&mut sink).await.unwrap_err();
    assert_eq!(error.cause, NativeError::InvalidResponse);
    assert_eq!(error.remote, RemoteOutcome::Unknown);
    assert!(fixture.driver.observe_next(&mut sink).await.is_err());
    drop(fixture.driver);
    assert_eq!(fixture.probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn managed_peer_loss_and_dropped_sink_future_never_replay_execution() {
    let mut fixture = managed_fixture().await;
    fixture.driver.submit().await.unwrap();
    received_request(&mut fixture.server).await;
    drop(fixture.server);
    let mut sink = Sink {
        released: Arc::new(AtomicBool::new(true)),
        outputs: 0,
    };
    let error = fixture.driver.observe_next(&mut sink).await.unwrap_err();
    assert_eq!(error.cause, NativeError::Transport);
    assert_eq!(error.remote, RemoteOutcome::Unknown);
    let writes = fixture.probe.writes.load(Ordering::SeqCst);
    assert!(fixture.driver.submit().await.is_err());
    assert_eq!(fixture.probe.writes.load(Ordering::SeqCst), writes);

    let mut fixture = managed_fixture().await;
    fixture.driver.submit().await.unwrap();
    let request = received_request(&mut fixture.server).await;
    queue_output_and_reply(&mut fixture.server, &request, "ok").await;
    let mut sink = Sink {
        released: Arc::new(AtomicBool::new(false)),
        outputs: 0,
    };
    let mut future = Box::pin(fixture.driver.observe_next(&mut sink));
    assert_pending(future.as_mut());
    drop(future);
    assert_eq!(fixture.driver.state(), DriverState::Interrupted);
    assert_eq!(fixture.driver.remote_outcome(), RemoteOutcome::Unknown);
    assert!(fixture.driver.observe_next(&mut sink).await.is_err());
    assert_eq!(fixture.receives.load(Ordering::SeqCst), 1);
    drop(fixture.driver);
    assert_eq!(fixture.probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn managed_constructor_rejects_another_notebook_owner_before_any_io() {
    let (io, _server, probe) = pair(1024);
    let channel = make_channel(io, options(2048), deadline()).await;
    let error = ManagedExecution::new(
        Box::new(channel),
        Box::new(TokioTimer),
        &request(),
        DriverOptions {
            ready: ready_for("other-notebook"),
            execution: ExecutionId::new("local").unwrap(),
            limits: DriverLimits::default(),
            deadline: deadline(),
        },
    )
    .unwrap_err();
    assert_eq!(error, NativeError::InvalidInput);
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_constructor_receive_and_close_dispose_without_io() {
    let expired = NativeDeadline::new(Duration::from_millis(1)).unwrap();
    while expired.remaining().is_ok() {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let (io, _server, probe) = pair(1024);
    assert_eq!(
        NotebookWebSocket::from_upgraded(io, ready(), options(256), expired.clone())
            .await
            .unwrap_err(),
        NativeError::TimedOut
    );
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    assert_eq!(
        channel.receive(256, &expired).await,
        Err(NativeError::TimedOut)
    );
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);

    let (io, _server, probe) = pair(1024);
    let mut channel = make_channel(io, options(256), deadline()).await;
    assert_eq!(channel.close(&expired).await, Err(NativeError::TimedOut));
    assert_eq!(channel.state(), NotebookWebSocketState::Closed);
    no_io(&probe);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finite_nonzero_write_threshold_and_read_buffer_boundaries_work() {
    for read_buffer_bytes in [1, 65536] {
        let (io, server, _) = pair(2048);
        let opts = NotebookWebSocketOptions {
            read_buffer_bytes,
            write_buffer_bytes: 256,
            ..options(256)
        };
        let mut channel = make_channel(io, opts, deadline()).await;
        let mut server = peer(server).await;
        channel.send(b"{}", &deadline()).await.unwrap();
        assert_eq!(server.next().await.unwrap().unwrap(), Message::text("{}"));
        let ping = vec![b'x'; 125];
        server
            .send(Message::Ping(ping.clone().into()))
            .await
            .unwrap();
        server.send(Message::text("{}")).await.unwrap();
        assert_eq!(
            channel.receive(256, &deadline()).await.unwrap(),
            Some(b"{}".to_vec())
        );
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::Pong(ping.into())
        );
    }
}
