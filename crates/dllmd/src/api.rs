use crate::{NetworkStore, StoreError};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dllm_protocol::{JoinToken, SignedState};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<NetworkStore>>,
    pub state_path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct InviteRequest {
    pub expires_at_unix: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    pub token: JoinToken,
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
        .with_state(state)
}

async fn status(State(state): State<ApiState>) -> Json<SignedState> {
    Json(state.store.lock().await.state.clone())
}

async fn invite(
    State(state): State<ApiState>,
    Json(request): Json<InviteRequest>,
) -> Json<JoinToken> {
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

fn key_bytes(bytes: Vec<u8>) -> Result<[u8; 32], ApiError> {
    bytes
        .try_into()
        .map_err(|_| ApiError::BadRequest("node_pubkey must contain 32 bytes"))
}

enum ApiError {
    BadRequest(&'static str),
    Store(StoreError),
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::Store(StoreError::WrongNetwork | StoreError::TokenUsed) => {
                (StatusCode::CONFLICT, "join token rejected").into_response()
            }
            Self::Store(error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
            }
        }
    }
}
