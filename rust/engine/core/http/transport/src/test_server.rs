// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    collections::VecDeque,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::TlsAcceptor;

trait Socket: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Socket for T {}

#[derive(Clone)]
pub(crate) enum Reply {
    Fixed(u16, Vec<(String, String)>, Vec<u8>),
    Stall,
    Endless,
    Close,
    Raw(Vec<u8>),
}

impl Reply {
    pub(crate) fn body(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::Fixed(status, Vec::new(), body.into())
    }

    pub(crate) fn headers(self, headers: &[(&str, &str)]) -> Self {
        let Self::Fixed(status, _, body) = self else {
            panic!("headers require a fixed test response")
        };
        Self::Fixed(
            status,
            headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body,
        )
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) requests: Mutex<Vec<Vec<u8>>>,
    pub(crate) connections: AtomicUsize,
    pub(crate) closed: AtomicUsize,
    pub(crate) tls_rejected: AtomicUsize,
    changed: Notify,
}

pub(crate) struct Server {
    pub(crate) address: SocketAddr,
    pub(crate) state: Arc<State>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    pub(crate) async fn start(replies: Vec<Reply>, tls: Option<TlsAcceptor>) -> Self {
        Self::start_mode(replies, tls, false).await
    }

    pub(crate) async fn proxy(replies: Vec<Reply>, tls: TlsAcceptor) -> Self {
        Self::start_mode(replies, Some(tls), true).await
    }

    async fn start_mode(replies: Vec<Reply>, tls: Option<TlsAcceptor>, proxy: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let shared = state.clone();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let task = tokio::spawn(async move {
            let serve = async {
                let mut connections = JoinSet::new();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let (mut socket, _) = accepted.unwrap();
                            let state = shared.clone();
                            assert!(state.connections.fetch_add(1, Ordering::SeqCst) < 64);
                            let tls = tls.clone();
                            let replies = replies.clone();
                            connections.spawn(async move {
                                if proxy {
                                    let connect = request_headers(&mut socket).await.unwrap();
                                    assert!(connect.starts_with(b"CONNECT api.fabric.microsoft.com:443 HTTP/1.1\r\n"));
                                    state.requests.lock().unwrap().push(connect);
                                    socket.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.unwrap();
                                    socket.flush().await.unwrap();
                                }
                                let socket: Box<dyn Socket> = if let Some(tls) = tls {
                                    match tls.accept(socket).await {
                                        Ok(socket) => Box::new(socket),
                                        Err(_) => {
                                            state.tls_rejected.fetch_add(1, Ordering::SeqCst);
                                            state.changed.notify_waiters();
                                            return;
                                        }
                                    }
                                } else {
                                    Box::new(socket)
                                };
                                match connection(socket, &state, &replies).await {
                                    Ok(()) => {}
                                    Err(error) => assert!(
                                        matches!(error.kind(),
                                            io::ErrorKind::BrokenPipe |
                                            io::ErrorKind::ConnectionAborted |
                                            io::ErrorKind::ConnectionReset |
                                            io::ErrorKind::UnexpectedEof
                                        ),
                                        "unexpected test-server I/O failure: {error}"
                                    ),
                                }
                                state.closed.fetch_add(1, Ordering::SeqCst);
                                state.changed.notify_waiters();
                            });
                        }
                        result = connections.join_next(), if !connections.is_empty() => {
                            result.unwrap().unwrap();
                        }
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(15), serve)
                .await
                .expect("offline test server exceeded its lifetime bound");
        });
        Self {
            address,
            state,
            task: Some(task),
        }
    }

    pub(crate) async fn wait_closed(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let changed = self.state.changed.notified();
                if self.state.closed.load(Ordering::SeqCst) > 0 {
                    return;
                }
                changed.await;
            }
        })
        .await
        .expect("client did not release the loopback connection");
    }

    pub(crate) fn requests(&self) -> Vec<Vec<u8>> {
        self.state.requests.lock().unwrap().clone()
    }

    pub(crate) async fn stop(mut self) {
        let task = self.task.take().unwrap();
        task.abort();
        // Awaiting the abort also drops the JoinSet and all attached connections.
        assert!(task.await.unwrap_err().is_cancelled());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn request_headers(socket: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut request = Vec::new();
    loop {
        request.push(socket.read_u8().await?);
        assert!(request.len() <= 16 * 1024, "oversized test request headers");
        if request.ends_with(b"\r\n\r\n") {
            return Ok(request);
        }
    }
}

async fn connection(
    mut socket: Box<dyn Socket>,
    state: &State,
    replies: &Mutex<VecDeque<Reply>>,
) -> io::Result<()> {
    for _ in 0..64 {
        let mut request = request_headers(&mut socket).await?;
        let header_end = request.len();
        let header = String::from_utf8(request.clone()).unwrap();
        let length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        assert!(length <= 1024 * 1024, "oversized test request body");
        request.resize(header_end + length, 0);
        socket.read_exact(&mut request[header_end..]).await?;
        state.requests.lock().unwrap().push(request);
        state.changed.notify_waiters();
        let reply = replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request or implicit replay");
        match reply {
            Reply::Fixed(status, headers, body) => {
                let mut head = format!("HTTP/1.1 {status} Synthetic\r\n");
                for (name, value) in headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
                socket.write_all(head.as_bytes()).await?;
                socket.write_all(&body).await?;
                socket.flush().await?;
            }
            Reply::Stall => {
                socket
                    .write_all(b"HTTP/1.1 200 Synthetic\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .await?;
                socket.flush().await?;
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).await?, 0);
                return Ok(());
            }
            Reply::Endless => {
                socket
                    .write_all(b"HTTP/1.1 200 Synthetic\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .await?;
                let chunk = [b'x'; 8192];
                loop {
                    socket.write_all(b"2000\r\n").await?;
                    socket.write_all(&chunk).await?;
                    socket.write_all(b"\r\n").await?;
                    socket.flush().await?;
                }
            }
            Reply::Close => return Ok(()),
            Reply::Raw(bytes) => {
                socket.write_all(&bytes).await?;
                socket.flush().await?;
            }
        }
    }
    panic!("test connection exceeded its request bound")
}
