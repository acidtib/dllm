use crate::{NetworkStore, StoreError};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use dllm_protocol::{HealthState, ManagementStatus, NodeStatus, SignedJoinToken};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<NetworkStore>>,
    pub state_path: PathBuf,
    pub runtime_url: Option<String>,
    pub admission: Arc<Semaphore>,
    pub client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct InviteRequest {
    pub expires_at_unix: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub token: SignedJoinToken,
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct RevokeRequest {
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct MutationResponse {
    generation: u64,
    changed: bool,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/invitations", post(invite))
        .route("/v1/members/join", post(join))
        .route("/v1/members/revoke", post(revoke))
        .route("/v1/models", get(proxy_models))
        .route("/v1/chat/completions", post(proxy_chat))
        .with_state(state)
}

async fn status(State(state): State<ApiState>) -> Json<ManagementStatus> {
    let store = state.store.lock().await;
    let mut nodes = vec![NodeStatus {
        node_pubkey: store.state.state.owner_pubkey,
        owner: true,
        health: HealthState::Ready,
    }];
    nodes.extend(store.state.state.members.iter().map(|member| NodeStatus {
        node_pubkey: member.node_pubkey,
        owner: false,
        health: HealthState::Unknown,
    }));
    Json(ManagementStatus {
        network: store.state.clone(),
        nodes,
        workers: vec![],
        placements: vec![],
        health: HealthState::Ready,
    })
}

async fn invite(
    State(state): State<ApiState>,
    Json(request): Json<InviteRequest>,
) -> Json<SignedJoinToken> {
    Json(
        state
            .store
            .lock()
            .await
            .issue_join_token(request.expires_at_unix),
    )
}

async fn join(
    State(state): State<ApiState>,
    Json(request): Json<JoinRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    store.redeem_join_token(request.token, node_pubkey)?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn revoke(
    State(state): State<ApiState>,
    Json(request): Json<RevokeRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.revoke_member(node_pubkey)?;
    if changed {
        store.save(&state.state_path)?;
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn proxy_models(State(state): State<ApiState>) -> Result<Response, ApiError> {
    proxy(
        state,
        reqwest::Method::GET,
        "/v1/models",
        HeaderMap::new(),
        Bytes::new(),
    )
    .await
}

async fn proxy_chat(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    proxy(
        state,
        reqwest::Method::POST,
        "/v1/chat/completions",
        headers,
        body,
    )
    .await
}

async fn proxy(
    state: ApiState,
    method: reqwest::Method,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let runtime_url = state
        .runtime_url
        .as_ref()
        .ok_or(ApiError::RuntimeUnavailable)?;
    let permit = state
        .admission
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Saturated)?;
    let mut request = state.client.request(method, format!("{runtime_url}{path}"));
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    if !body.is_empty() {
        request = request.body(body);
    }
    let upstream = request.send().await.map_err(ApiError::Runtime)?;
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let stream = upstream.bytes_stream().map(move |item| {
        let _permit = &permit;
        item.map_err(std::io::Error::other)
    });
    let mut response = Response::builder().status(status);
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    response
        .body(Body::from_stream(stream))
        .map_err(|_| ApiError::BadRequest("invalid upstream response"))
}

fn key_bytes(bytes: Vec<u8>) -> Result<[u8; 32], ApiError> {
    bytes
        .try_into()
        .map_err(|_| ApiError::BadRequest("node_pubkey must contain 32 bytes"))
}

enum ApiError {
    BadRequest(&'static str),
    Store(StoreError),
    RuntimeUnavailable,
    Saturated,
    Runtime(reqwest::Error),
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::Store(
                StoreError::WrongNetwork | StoreError::TokenUsed | StoreError::Token(_),
            ) => (StatusCode::CONFLICT, "join token rejected").into_response(),
            Self::Store(error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
            }
            Self::RuntimeUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "model runtime is not configured",
            )
                .into_response(),
            Self::Saturated => (
                StatusCode::TOO_MANY_REQUESTS,
                "inference admission queue is saturated",
            )
                .into_response(),
            Self::Runtime(error) => (StatusCode::BAD_GATEWAY, error.to_string()).into_response(),
        }
    }
}
