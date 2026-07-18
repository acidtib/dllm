use crate::api::ApiState;
use dllm_protocol::{now_ms, now_unix};
use dllm_transport::{
    auth::{AuthError, AuthView, PeerAuth},
    peer::{PeerId, PeerNodeHandle},
    protocol::{self, ErrorCode, FrameCodec, Message, PROTOCOL_VERSION},
    stream_handler::{AppEvent, OpenStreamCmd},
};
use futures_util::{AsyncReadExt, AsyncWriteExt};
use libp2p::Stream;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{oneshot, Mutex, Semaphore};

async fn read_frame(stream: &mut Stream) -> Result<Message, String> {
    let mut header = [0u8; 6];
    read_exact(stream, &mut header)
        .await
        .map_err(|e| format!("read header: {e}"))?;
    if header[0] != PROTOCOL_VERSION {
        return Err(format!("unknown protocol version {}", header[0]));
    }
    let _msg_type = protocol::MessageType::from_byte(header[1])
        .ok_or_else(|| format!("unknown message type {}", header[1]))?;
    let payload_len = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    if payload_len > protocol::MAX_FRAME_SIZE {
        return Err(format!("oversized frame: {payload_len}"));
    }
    let mut payload = vec![0u8; payload_len];
    read_exact(stream, &mut payload)
        .await
        .map_err(|e| format!("read payload: {e}"))?;
    let now = now_ms();
    let codec = FrameCodec::default();
    let full_frame = [&header[..], &payload[..]].concat();
    codec
        .decode(&full_frame, now)
        .map_err(|e| format!("decode: {e:?}"))
}

async fn write_frame(stream: &mut Stream, message: &Message) -> Result<(), String> {
    let codec = FrameCodec::default();
    let frame = codec.encode(message);
    write_all(stream, &frame)
        .await
        .map_err(|e| format!("write frame: {e}"))
}

async fn read_exact(stream: &mut Stream, buf: &mut [u8]) -> Result<(), std::io::Error> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = stream
            .read(&mut buf[offset..])
            .await
            .map_err(std::io::Error::other)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "stream closed",
            ));
        }
        offset += n;
    }
    Ok(())
}

async fn write_all(stream: &mut Stream, buf: &[u8]) -> Result<(), String> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = stream
            .write(&buf[offset..])
            .await
            .map_err(|e| format!("write: {e}"))?;
        if n == 0 {
            return Err("stream closed".into());
        }
        offset += n;
    }
    Ok(())
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Stream, String>>>>>;

#[derive(Clone)]
pub struct PeerClient {
    handle: PeerNodeHandle,
    auth: AuthView,
    admission: Arc<Semaphore>,
    pending_outbound: PendingMap,
    next_tag: Arc<AtomicU64>,
}

impl PeerClient {
    pub fn new(handle: PeerNodeHandle, auth: AuthView, admission: Arc<Semaphore>) -> Self {
        Self {
            handle,
            auth,
            admission,
            pending_outbound: Arc::new(Mutex::new(HashMap::new())),
            next_tag: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn auth(&self) -> &AuthView {
        &self.auth
    }

    /// Open a stream to a peer and return it.
    async fn open_stream_to(&self, peer_id: PeerId) -> Result<Stream, String> {
        let tag = self.next_tag.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_outbound.lock().await.insert(tag, tx);
        self.handle.open_stream(OpenStreamCmd { peer_id, tag });
        match rx.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(e)) => {
                self.pending_outbound.lock().await.remove(&tag);
                Err(e)
            }
            Err(_) => {
                self.pending_outbound.lock().await.remove(&tag);
                Err("stream request cancelled".into())
            }
        }
    }

    /// Perform an authenticated health check against a peer.
    pub async fn health_check(&self, peer_id: PeerId) -> Result<(), String> {
        let mut stream = self.open_stream_to(peer_id).await?;
        write_frame(&mut stream, &Message::HealthRequest).await?;
        match read_frame(&mut stream).await? {
            Message::HealthResponse => Ok(()),
            Message::Error { code, message, .. } => {
                Err(format!("health rejected: {code:?} {message}"))
            }
            other => Err(format!("unexpected health response: {other:?}")),
        }
    }
}

/// Everything ApiState needs once the local node's transport identity is
/// authorized: diagnostics, the client used to reach other peers, and the
/// auth view used to resolve peer IDs. Always populated or cleared
/// together, since they share one lifecycle (the running libp2p node).
#[derive(Clone)]
pub struct PeerBundle {
    pub diagnostics: tokio::sync::watch::Receiver<dllm_transport::peer::PeerDiagnostics>,
    pub client: PeerClient,
    pub auth_view: AuthView,
}

/// Spawn a background task that dispatches incoming stream events.
pub fn spawn_dispatcher(peer_client: PeerClient, state: ApiState) -> tokio::task::JoinHandle<()> {
    let handle = peer_client.handle.clone();
    let pending = peer_client.pending_outbound.clone();
    tokio::spawn(async move {
        loop {
            match handle.recv_stream_event().await {
                Some(AppEvent::Inbound { peer, stream }) => {
                    handle.update_diagnostics(|d| {
                        d.active_inbound_streams += 1;
                        d.last_stream_peer = Some(peer.to_string());
                    });
                    let client = peer_client.clone();
                    let st = state.clone();
                    tokio::spawn(async move {
                        handle_inbound(peer, stream, client, st).await;
                    });
                }
                Some(AppEvent::OutboundReady { stream, tag }) => {
                    handle.update_diagnostics(|d| {
                        d.active_outbound_streams += 1;
                    });
                    if let Some(tx) = pending.lock().await.remove(&tag) {
                        let _ = tx.send(Ok(stream));
                    }
                }
                Some(AppEvent::OutboundError { tag }) => {
                    handle.update_diagnostics(|d| {
                        d.rejected_streams += 1;
                    });
                    if let Some(tx) = pending.lock().await.remove(&tag) {
                        let _ = tx.send(Err("peer stream open failed".into()));
                    }
                }
                None => break,
            }
        }
    })
}

async fn handle_inbound(peer: PeerId, mut stream: Stream, client: PeerClient, state: ApiState) {
    let dispatch = match inbound_dispatch(&mut stream, &peer, &client.auth).await {
        Ok(d) => d,
        Err(_e) => {
            client.handle.update_diagnostics(|d| {
                d.protocol_failures += 1;
                d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
            });
            return;
        }
    };

    match dispatch.auth_result {
        Err(e) => {
            client.handle.update_diagnostics(|d| {
                d.auth_failures += 1;
                d.last_app_error = Some(format!("auth: {e}"));
            });
            let _ = write_frame(
                &mut stream,
                &Message::Error {
                    request_id: 0,
                    code: ErrorCode::AuthFailed,
                    message: format!("{e}"),
                },
            )
            .await;
        }
        Ok(auth) => match dispatch.message {
            Message::HealthRequest => {
                let _ = write_frame(&mut stream, &Message::HealthResponse).await;
            }
            Message::InferenceStart {
                request_id,
                method,
                path,
                headers,
                body,
                deadline_unix_ms,
            } => {
                serve_inference(
                    &client,
                    &state,
                    &mut stream,
                    request_id,
                    &method,
                    &path,
                    &headers,
                    &body,
                    deadline_unix_ms,
                    auth.node_pubkey,
                )
                .await;
            }
            other => {
                let _ = write_frame(
                    &mut stream,
                    &Message::Error {
                        request_id: 0,
                        code: ErrorCode::InvalidTransition,
                        message: format!("unexpected first message: {other:?}"),
                    },
                )
                .await;
            }
        },
    }
}

struct InboundDispatch {
    auth_result: Result<PeerAuth, AuthError>,
    message: Message,
}

async fn inbound_dispatch(
    stream: &mut Stream,
    peer: &PeerId,
    auth: &AuthView,
) -> Result<InboundDispatch, String> {
    let message = read_frame(stream).await?;
    let auth_result = auth.authorize(peer, now_unix());
    Ok(InboundDispatch {
        auth_result,
        message,
    })
}

#[allow(clippy::too_many_arguments)]
async fn serve_inference(
    client: &PeerClient,
    state: &ApiState,
    stream: &mut Stream,
    request_id: u64,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    deadline_ms: u64,
    node_pubkey: [u8; 32],
) {
    let now = now_ms();
    if deadline_ms <= now {
        let _ = write_frame(
            stream,
            &Message::Error {
                request_id,
                code: ErrorCode::DeadlineExceeded,
                message: "deadline already expired".into(),
            },
        )
        .await;
        client.handle.update_diagnostics(|d| {
            d.deadline_expirations += 1;
            d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
        });
        return;
    }

    // Budget enforcement gate: no budget entry → fail closed.
    let _budget_permit = match state
        .budget_enforcer
        .try_admit(&client.auth.snapshot(), &node_pubkey)
        .await
    {
        Ok(permit) => permit,
        Err(_) => {
            let _ = write_frame(
                stream,
                &Message::Error {
                    request_id,
                    code: ErrorCode::CapacityExceeded,
                    message: "resource budget exhausted or absent".into(),
                },
            )
            .await;
            client.handle.update_diagnostics(|d| {
                d.rejected_streams += 1;
                d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
            });
            return;
        }
    };

    let permit = match client.admission.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            let _ = write_frame(
                stream,
                &Message::Error {
                    request_id,
                    code: ErrorCode::CapacityExceeded,
                    message: "admission saturated".into(),
                },
            )
            .await;
            client.handle.update_diagnostics(|d| {
                d.rejected_streams += 1;
                d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
            });
            return;
        }
    };

    let runtime_url = match state.runtime_url.as_ref() {
        Some(url) => url.clone(),
        None => {
            let _ = write_frame(
                stream,
                &Message::Error {
                    request_id,
                    code: ErrorCode::RuntimeError,
                    message: "no local runtime configured".into(),
                },
            )
            .await;
            client.handle.update_diagnostics(|d| {
                d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
            });
            return;
        }
    };

    let upstream_path = if path.starts_with("/v1/") {
        path.to_owned()
    } else {
        "/v1/chat/completions".to_string()
    };

    let mut req = state
        .client
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST),
            format!("{runtime_url}{upstream_path}"),
        )
        .timeout(std::time::Duration::from_millis(
            deadline_ms.saturating_sub(now),
        ));

    if let Some(ct) = headers.get("content-type") {
        req = req.header("content-type", ct);
    }
    if !body.is_empty() {
        req = req.body(body.to_vec());
    }

    match req.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let ct = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let response_headers = {
                let mut h = HashMap::new();
                if let Some(ref ct) = ct {
                    h.insert("content-type".into(), ct.clone());
                }
                h
            };

            if write_frame(
                stream,
                &Message::ResponseStart {
                    request_id,
                    status_code: status,
                    headers: response_headers,
                },
            )
            .await
            .is_err()
            {
                drop(permit);

                return;
            }

            let mut byte_stream = response.bytes_stream();
            loop {
                let chunk = tokio::select! {
                    chunk = futures_util::StreamExt::next(&mut byte_stream) => chunk,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(
                        deadline_ms.saturating_sub(now_ms()),
                    )) => {
                        let _ = write_frame(
                            stream,
                            &Message::Error {
                                request_id,
                                code: ErrorCode::DeadlineExceeded,
                                message: "deadline expired during streaming".into(),
                            },
                        )
                        .await;
                        client.handle.update_diagnostics(|d| {
                            d.deadline_expirations += 1;
                        });
                        break;
                    }
                };

                match chunk {
                    Some(Ok(bytes)) => {
                        if write_frame(
                            stream,
                            &Message::ResponseChunk {
                                request_id,
                                data: bytes.to_vec(),
                            },
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Some(Err(_)) => {
                        let _ = write_frame(
                            stream,
                            &Message::Error {
                                request_id,
                                code: ErrorCode::RuntimeError,
                                message: "upstream runtime read error".into(),
                            },
                        )
                        .await;
                        break;
                    }
                    None => {
                        let _ = write_frame(stream, &Message::End { request_id }).await;
                        break;
                    }
                }
            }
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Message::Error {
                    request_id,
                    code: ErrorCode::RuntimeError,
                    message: "upstream runtime unreachable".into(),
                },
            )
            .await;
        }
    }

    drop(permit);
    client.handle.update_diagnostics(|d| {
        d.active_inbound_streams = d.active_inbound_streams.saturating_sub(1);
    });
}

/// A streaming response from a remote peer.
pub struct PeerInferenceStream {
    stream: Stream,
    request_id: u64,
    response_status: u16,
    response_headers: HashMap<String, String>,
    done: bool,
}

impl PeerInferenceStream {
    pub fn status(&self) -> u16 {
        self.response_status
    }

    pub fn content_type(&self) -> Option<String> {
        self.response_headers.get("content-type").cloned()
    }

    /// Read the next chunk. Returns `None` when the stream ends.
    pub async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.done {
            return Ok(None);
        }
        match read_frame(&mut self.stream).await? {
            Message::ResponseChunk { data, .. } => Ok(Some(data)),
            Message::End { .. } => {
                self.done = true;
                Ok(None)
            }
            Message::Error { code, message, .. } => {
                self.done = true;
                Err(format!("peer error: {code:?} {message}"))
            }
            other => {
                self.done = true;
                Err(format!("unexpected frame: {other:?}"))
            }
        }
    }

    /// Send a cancellation to the serving peer.
    pub async fn cancel(&mut self) -> Result<(), String> {
        if !self.done {
            write_frame(
                &mut self.stream,
                &Message::Cancel {
                    request_id: self.request_id,
                },
            )
            .await?;
            self.done = true;
        }
        Ok(())
    }
}

/// Open an inference stream to a peer and send the start frame.
pub async fn open_peer_inference(
    client: &PeerClient,
    peer_id: PeerId,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    deadline_ms: u64,
) -> Result<PeerInferenceStream, String> {
    let mut stream = client.open_stream_to(peer_id).await?;
    let request_id = client.next_tag.fetch_add(1, Ordering::Relaxed);

    write_frame(
        &mut stream,
        &Message::InferenceStart {
            request_id,
            method: method.to_owned(),
            path: path.to_owned(),
            headers: headers.clone(),
            body: body.to_vec(),
            deadline_unix_ms: deadline_ms,
        },
    )
    .await?;

    match read_frame(&mut stream).await? {
        Message::ResponseStart {
            request_id: _,
            status_code,
            headers,
        } => Ok(PeerInferenceStream {
            stream,
            request_id,
            response_status: status_code,
            response_headers: headers,
            done: false,
        }),
        Message::Error { code, message, .. } => {
            Err(format!("peer rejected request: {code:?} {message}"))
        }
        other => Err(format!("unexpected response start: {other:?}")),
    }
}
