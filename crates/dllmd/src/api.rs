use crate::{NetworkStore, StoreError};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use dllm_protocol::{
    HealthState, ManagementStatus, NodeStatus, PlacementStatus, SignedJoinToken, WorkerStatus,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<NetworkStore>>,
    pub state_path: PathBuf,
    pub runtime_url: Option<String>,
    pub admission: Arc<Semaphore>,
    pub client: reqwest::Client,
    pub management_token: Option<String>,
    pub api_key: Option<String>,
    pub metrics: Arc<Metrics>,
    pub public_url: String,
}

#[derive(Default)]
pub struct Metrics {
    inference_requests: AtomicU64,
    admission_rejections: AtomicU64,
    upstream_failures: AtomicU64,
    response_bytes: AtomicU64,
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

#[derive(Debug, Deserialize)]
pub struct AssignmentRequest {
    pub model: String,
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct MutationResponse {
    generation: u64,
    changed: bool,
}

pub fn router(state: ApiState) -> Router {
    let mut management = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/invitations", post(invite))
        .route("/v1/members/revoke", post(revoke))
        .route("/v1/assignments", post(assign).delete(unassign));
    if let Some(token) = state.management_token.clone() {
        management = management.route_layer(middleware::from_fn_with_state(token, require_bearer));
    }
    let mut inference = Router::new()
        .route("/v1/models", get(proxy_models))
        .route("/v1/chat/completions", post(proxy_chat));
    if let Some(token) = state.api_key.clone() {
        inference = inference.route_layer(middleware::from_fn_with_state(token, require_bearer));
    }
    Router::new()
        .route("/", get(ui))
        .route("/metrics", get(metrics))
        .route("/v1/members/join", post(join))
        .merge(management)
        .merge(inference)
        .with_state(state)
}

async fn metrics(State(state): State<ApiState>) -> String {
    format!(
        concat!(
            "dllm_inference_requests_total {}\n",
            "dllm_admission_rejections_total {}\n",
            "dllm_upstream_failures_total {}\n",
            "dllm_response_bytes_total {}\n",
            "dllm_admission_available_permits {}\n"
        ),
        state.metrics.inference_requests.load(Ordering::Relaxed),
        state.metrics.admission_rejections.load(Ordering::Relaxed),
        state.metrics.upstream_failures.load(Ordering::Relaxed),
        state.metrics.response_bytes.load(Ordering::Relaxed),
        state.admission.available_permits(),
    )
}

async fn require_bearer(
    State(expected): State<String>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied != Some(expected.as_str()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

async fn ui() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../../web/index.html"),
    )
}

async fn status(State(state): State<ApiState>) -> Json<ManagementStatus> {
    let runtime_ready = match state.runtime_url.as_ref() {
        Some(url) => state
            .client
            .get(format!("{url}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success()),
        None => false,
    };
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
    let placement_health = if runtime_ready {
        HealthState::Ready
    } else {
        HealthState::Unavailable
    };
    let workers = store
        .state
        .state
        .placements
        .iter()
        .map(|placement| WorkerStatus {
            worker_id: placement.placement_id,
            node_pubkey: placement.node_pubkey,
            model: placement.model.clone(),
            health: placement_health.clone(),
        })
        .collect();
    let placements = store
        .state
        .state
        .placements
        .iter()
        .map(|placement| PlacementStatus {
            placement_id: placement.placement_id,
            model: placement.model.clone(),
            generation: placement.created_generation,
            worker_ids: vec![placement.placement_id],
            health: placement_health.clone(),
        })
        .collect::<Vec<_>>();
    let health = if placements
        .iter()
        .any(|placement| placement.health == HealthState::Unavailable)
    {
        HealthState::Degraded
    } else {
        HealthState::Ready
    };
    Json(ManagementStatus {
        network: store.state.clone(),
        nodes,
        workers,
        placements,
        health,
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
            .issue_join_token(state.public_url.clone(), request.expires_at_unix),
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

async fn assign(
    State(state): State<ApiState>,
    Json(request): Json<AssignmentRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.assign_model(request.model, node_pubkey)?;
    if changed {
        store.save(&state.state_path)?;
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn unassign(
    State(state): State<ApiState>,
    Json(request): Json<AssignmentRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.unassign_model(&request.model, node_pubkey)?;
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
    state
        .metrics
        .inference_requests
        .fetch_add(1, Ordering::Relaxed);
    let runtime_url = state
        .runtime_url
        .as_ref()
        .ok_or(ApiError::RuntimeUnavailable)?;
    let permit = state.admission.clone().try_acquire_owned().map_err(|_| {
        state
            .metrics
            .admission_rejections
            .fetch_add(1, Ordering::Relaxed);
        ApiError::Saturated
    })?;
    let mut request = state.client.request(method, format!("{runtime_url}{path}"));
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    if !body.is_empty() {
        request = request.body(body);
    }
    let upstream = request.send().await.map_err(|error| {
        state
            .metrics
            .upstream_failures
            .fetch_add(1, Ordering::Relaxed);
        ApiError::Runtime(error)
    })?;
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let metrics = state.metrics.clone();
    let stream = upstream.bytes_stream().map(move |item| {
        let _permit = &permit;
        if let Ok(bytes) = &item {
            metrics
                .response_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
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
            Self::Store(StoreError::AssignmentNodeUnknown) => (
                StatusCode::BAD_REQUEST,
                "assignment node is not a network member",
            )
                .into_response(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state(management_token: Option<&str>, runtime_url: Option<String>) -> ApiState {
        ApiState {
            store: Arc::new(Mutex::new(NetworkStore::create("test"))),
            state_path: std::env::temp_dir().join("dllmd-api-test-state.json"),
            runtime_url,
            admission: Arc::new(Semaphore::new(1)),
            client: reqwest::Client::new(),
            management_token: management_token.map(str::to_owned),
            api_key: None,
            metrics: Arc::new(Metrics::default()),
            public_url: "http://127.0.0.1:7337".into(),
        }
    }

    #[tokio::test]
    async fn management_token_is_enforced() {
        let app = router(state(Some("secret"), None));
        let unauthorized = app
            .clone()
            .oneshot(Request::get("/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let authorized = app
            .oneshot(
                Request::get("/v1/status")
                    .header(header::AUTHORIZATION, "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn model_proxy_streams_and_counts_bytes() {
        let upstream = Router::new().route(
            "/v1/models",
            get(|| async { Json(serde_json::json!({"object":"list","data":[]})) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let state = state(None, Some(format!("http://{address}")));
        let metrics = state.metrics.clone();
        let response = router(state)
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.starts_with(b"{"));
        assert_eq!(metrics.inference_requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            metrics.response_bytes.load(Ordering::Relaxed),
            body.len() as u64
        );
    }

    #[tokio::test]
    async fn inference_api_key_and_saturation_are_enforced() {
        let mut state = state(None, Some("http://127.0.0.1:1".into()));
        state.api_key = Some("inference-secret".into());
        let permit = state.admission.clone().acquire_owned().await.unwrap();
        let metrics = state.metrics.clone();
        let app = router(state);
        let unauthorized = app
            .clone()
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let saturated = app
            .oneshot(
                Request::get("/v1/models")
                    .header(header::AUTHORIZATION, "Bearer inference-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        drop(permit);
        assert_eq!(saturated.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(metrics.admission_rejections.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn chat_proxy_preserves_streaming_content_type() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    "data: {\"choices\":[]}\n\ndata: [DONE]\n\n",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let response = router(state(None, Some(format!("http://{address}"))))
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"stream\":true}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/event-stream"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.ends_with(b"data: [DONE]\n\n"));
    }
}
