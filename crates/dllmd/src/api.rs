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
use futures_util::{future::join_all, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
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
    pub peer_api_key: Option<String>,
    pub metrics: Arc<Metrics>,
    pub public_url: String,
    pub replica_loads: Arc<Mutex<HashMap<uuid::Uuid, Arc<AtomicU64>>>>,
}

#[derive(Default)]
pub struct Metrics {
    inference_requests: AtomicU64,
    admission_rejections: AtomicU64,
    upstream_failures: AtomicU64,
    request_bytes: AtomicU64,
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
    pub node_endpoint: String,
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
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/health/runtime", get(runtime_health))
        .route("/metrics", get(metrics))
        .route("/v1/members/join", post(join))
        .merge(management)
        .merge(inference)
        .with_state(state)
}

async fn runtime_health(State(state): State<ApiState>) -> StatusCode {
    if runtime_is_ready(&state).await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

async fn runtime_is_ready(state: &ApiState) -> bool {
    match state.runtime_url.as_ref() {
        Some(url) => state
            .client
            .get(format!("{url}/health"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success()),
        None => false,
    }
}

async fn metrics(State(state): State<ApiState>) -> String {
    let mut output = format!(
        concat!(
            "dllm_inference_requests_total {}\n",
            "dllm_admission_rejections_total {}\n",
            "dllm_upstream_failures_total {}\n",
            "dllm_request_bytes_total {}\n",
            "dllm_response_bytes_total {}\n",
            "dllm_admission_available_permits {}\n"
        ),
        state.metrics.inference_requests.load(Ordering::Relaxed),
        state.metrics.admission_rejections.load(Ordering::Relaxed),
        state.metrics.upstream_failures.load(Ordering::Relaxed),
        state.metrics.request_bytes.load(Ordering::Relaxed),
        state.metrics.response_bytes.load(Ordering::Relaxed),
        state.admission.available_permits(),
    );
    let loads = state.replica_loads.lock().await;
    let mut placements = loads.iter().collect::<Vec<_>>();
    placements.sort_by_key(|(placement_id, _)| **placement_id);
    for (placement_id, load) in placements {
        output.push_str(&format!(
            "dllm_replica_in_flight{{placement_id=\"{placement_id}\"}} {}\n",
            load.load(Ordering::Relaxed)
        ));
    }
    output
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
    let runtime_ready = runtime_is_ready(&state).await;
    let network = state.store.lock().await.state.clone();
    let mut nodes = vec![NodeStatus {
        node_pubkey: network.state.owner_pubkey,
        endpoint: state.public_url.clone(),
        owner: true,
        health: HealthState::Ready,
    }];
    let probes = network.state.members.iter().map(|member| {
        let client = state.client.clone();
        let member = member.clone();
        async move {
            let ready = client
                .get(format!("{}/health", member.endpoint))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            NodeStatus {
                node_pubkey: member.node_pubkey,
                endpoint: member.endpoint,
                owner: false,
                health: if ready {
                    HealthState::Ready
                } else {
                    HealthState::Unavailable
                },
            }
        }
    });
    nodes.extend(join_all(probes).await);
    let runtime_probes = network.state.members.iter().map(|member| {
        let client = state.client.clone();
        let member = member.clone();
        async move {
            let ready = client
                .get(format!("{}/health/runtime", member.endpoint))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            (member.node_pubkey, ready)
        }
    });
    let member_runtime_health = join_all(runtime_probes).await;
    let placement_health = |node_pubkey: &[u8; 32]| {
        if node_pubkey == &network.state.owner_pubkey {
            if runtime_ready {
                HealthState::Ready
            } else {
                HealthState::Unavailable
            }
        } else {
            member_runtime_health
                .iter()
                .find(|(candidate, _)| candidate == node_pubkey)
                .map(|(_, ready)| {
                    if *ready {
                        HealthState::Ready
                    } else {
                        HealthState::Unavailable
                    }
                })
                .unwrap_or(HealthState::Unavailable)
        }
    };
    let workers = network
        .state
        .placements
        .iter()
        .map(|placement| WorkerStatus {
            worker_id: placement.placement_id,
            node_pubkey: placement.node_pubkey,
            model: placement.model.clone(),
            health: placement_health(&placement.node_pubkey),
        })
        .collect();
    let placements = network
        .state
        .placements
        .iter()
        .map(|placement| PlacementStatus {
            placement_id: placement.placement_id,
            model: placement.model.clone(),
            generation: placement.created_generation,
            worker_ids: vec![placement.placement_id],
            health: placement_health(&placement.node_pubkey),
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
        network,
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
    store.redeem_join_token(request.token, node_pubkey, request.node_endpoint)?;
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

async fn proxy_models(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let store = state.store.lock().await;
    let mut models = store
        .state
        .state
        .model_assignments
        .iter()
        .map(|assignment| assignment.model.clone())
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    let data = models
        .into_iter()
        .map(|model| {
            serde_json::json!({
                "id": model,
                "object": "model",
                "owned_by": "dllm"
            })
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "object": "list", "data": data }))
}

async fn proxy_chat(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let replica = resolve_runtime(&state, &body).await?;
    proxy(
        state,
        replica,
        reqwest::Method::POST,
        "/v1/chat/completions",
        headers,
        body,
    )
    .await
}

async fn proxy(
    state: ApiState,
    replica: ResolvedReplica,
    method: reqwest::Method,
    path: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    state
        .metrics
        .inference_requests
        .fetch_add(1, Ordering::Relaxed);
    state
        .metrics
        .request_bytes
        .fetch_add(body.len() as u64, Ordering::Relaxed);
    let permit = state.admission.clone().try_acquire_owned().map_err(|_| {
        state
            .metrics
            .admission_rejections
            .fetch_add(1, Ordering::Relaxed);
        ApiError::Saturated
    })?;
    let mut request = state
        .client
        .request(method, format!("{}{path}", replica.runtime_url));
    if replica.peer {
        if let Some(key) = &state.peer_api_key {
            request = request.bearer_auth(key);
        }
    }
    if let Some(content_type) = headers.get(header::CONTENT_TYPE) {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    if !body.is_empty() {
        request = request.body(body);
    }
    let replica_lease = ReplicaLease::new(replica.in_flight);
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
        let _replica_lease = &replica_lease;
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

struct ResolvedReplica {
    runtime_url: String,
    peer: bool,
    in_flight: Arc<AtomicU64>,
}

struct ReplicaLease(Arc<AtomicU64>);

impl ReplicaLease {
    fn new(load: Arc<AtomicU64>) -> Self {
        load.fetch_add(1, Ordering::Relaxed);
        Self(load)
    }
}

impl Drop for ReplicaLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn resolve_runtime(state: &ApiState, body: &Bytes) -> Result<ResolvedReplica, ApiError> {
    let request: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| ApiError::BadRequest("invalid JSON request"))?;
    let model = request
        .get("model")
        .and_then(|value| value.as_str())
        .ok_or(ApiError::BadRequest("model is required"))?;
    let (owner, placements, members) = {
        let store = state.store.lock().await;
        (
            store.state.state.owner_pubkey,
            store
                .state
                .state
                .placements
                .iter()
                .filter(|placement| placement.model == model)
                .cloned()
                .collect::<Vec<_>>(),
            store.state.state.members.clone(),
        )
    };
    if placements.is_empty() {
        return Err(ApiError::ModelUnavailable);
    }
    let mut candidates = Vec::new();
    for placement in placements {
        let (runtime_url, peer, ready) = if placement.node_pubkey == owner {
            (
                state.runtime_url.clone().unwrap_or_default(),
                false,
                runtime_is_ready(state).await,
            )
        } else if let Some(member) = members
            .iter()
            .find(|member| member.node_pubkey == placement.node_pubkey)
        {
            let ready = state
                .client
                .get(format!("{}/health/runtime", member.endpoint))
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            (member.endpoint.clone(), true, ready)
        } else {
            continue;
        };
        if ready {
            let load = state
                .replica_loads
                .lock()
                .await
                .entry(placement.placement_id)
                .or_default()
                .clone();
            candidates.push((
                load.load(Ordering::Relaxed),
                placement.placement_id,
                runtime_url,
                peer,
                load,
            ));
        }
    }
    candidates.sort_by_key(|(load, placement_id, ..)| (*load, *placement_id));
    candidates
        .into_iter()
        .next()
        .map(|(_, _, runtime_url, peer, in_flight)| ResolvedReplica {
            runtime_url,
            peer,
            in_flight,
        })
        .ok_or(ApiError::RuntimeUnavailable)
}

fn key_bytes(bytes: Vec<u8>) -> Result<[u8; 32], ApiError> {
    bytes
        .try_into()
        .map_err(|_| ApiError::BadRequest("node_pubkey must contain 32 bytes"))
}

#[derive(Debug)]
enum ApiError {
    BadRequest(&'static str),
    Store(StoreError),
    RuntimeUnavailable,
    Saturated,
    Runtime(reqwest::Error),
    ModelUnavailable,
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
            Self::ModelUnavailable => {
                (StatusCode::NOT_FOUND, "model has no available placement").into_response()
            }
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
            peer_api_key: None,
            metrics: Arc::new(Metrics::default()),
            public_url: "http://127.0.0.1:7337".into(),
            replica_loads: Arc::new(Mutex::new(HashMap::new())),
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
    async fn model_listing_reflects_assignments() {
        let state = state(None, None);
        let owner = state.store.lock().await.state.state.owner_pubkey;
        state
            .store
            .lock()
            .await
            .assign_model("qwen".into(), owner)
            .unwrap();
        let response = router(state)
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.windows(4).any(|window| window == b"qwen"));
    }

    #[tokio::test]
    async fn inference_api_key_and_saturation_are_enforced() {
        let upstream = Router::new().route("/health", get(|| async { StatusCode::OK }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut state = state(None, Some(format!("http://{address}")));
        state.api_key = Some("inference-secret".into());
        let owner = state.store.lock().await.state.state.owner_pubkey;
        state
            .store
            .lock()
            .await
            .assign_model("qwen".into(), owner)
            .unwrap();
        let permit = state.admission.clone().acquire_owned().await.unwrap();
        let metrics = state.metrics.clone();
        let app = router(state);
        let unauthorized = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .body(Body::from("{\"model\":\"qwen\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let saturated = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer inference-secret")
                    .body(Body::from("{\"model\":\"qwen\"}"))
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
        let upstream = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .route(
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
        let state = state(None, Some(format!("http://{address}")));
        let owner = state.store.lock().await.state.state.owner_pubkey;
        state
            .store
            .lock()
            .await
            .assign_model("qwen".into(), owner)
            .unwrap();
        let app = router(state);
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"model\":\"qwen\",\"stream\":true}"))
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

    #[tokio::test]
    async fn chat_routes_to_assigned_member_with_peer_key() {
        let upstream = Router::new()
            .route("/health/runtime", get(|| async { StatusCode::OK }))
            .route(
                "/v1/chat/completions",
                post(|headers: HeaderMap| async move {
                    if headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        != Some("Bearer peer-secret")
                    {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    Json(serde_json::json!({"choices":[{"message":{"content":"remote"}}]}))
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut state = state(None, None);
        state.peer_api_key = Some("peer-secret".into());
        let endpoint = format!("http://{address}");
        let member = NetworkStore::random_node_key();
        {
            let mut store = state.store.lock().await;
            let token = store.issue_join_token("http://owner".into(), None);
            store.redeem_join_token(token, member, endpoint).unwrap();
            store.assign_model("qwen".into(), member).unwrap();
        }
        let shared_store = state.store.clone();
        let app = router(state);
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"model\":\"qwen\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(body.windows(6).any(|window| window == b"remote"));
        shared_store.lock().await.revoke_member(member).unwrap();
        let unavailable = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"model\":\"qwen\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn replica_routing_prefers_ready_replica_with_less_load() {
        async fn replica(ready: bool) -> String {
            let health = if ready {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            let app = Router::new().route("/health/runtime", get(move || async move { health }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            format!("http://{address}")
        }

        let unavailable_endpoint = replica(false).await;
        let busy_endpoint = replica(true).await;
        let ready_endpoint = replica(true).await;
        let state = state(None, None);
        let first = NetworkStore::random_node_key();
        let second = NetworkStore::random_node_key();
        let third = NetworkStore::random_node_key();
        let (first_placement, second_placement, third_placement) = {
            let mut store = state.store.lock().await;
            let first_token = store.issue_join_token("http://owner".into(), None);
            store
                .redeem_join_token(first_token, first, unavailable_endpoint)
                .unwrap();
            let second_token = store.issue_join_token("http://owner".into(), None);
            store
                .redeem_join_token(second_token, second, busy_endpoint)
                .unwrap();
            let third_token = store.issue_join_token("http://owner".into(), None);
            store
                .redeem_join_token(third_token, third, ready_endpoint.clone())
                .unwrap();
            store.assign_model("qwen".into(), first).unwrap();
            store.assign_model("qwen".into(), second).unwrap();
            store.assign_model("qwen".into(), third).unwrap();
            (
                store.state.state.placements[0].placement_id,
                store.state.state.placements[1].placement_id,
                store.state.state.placements[2].placement_id,
            )
        };
        state
            .replica_loads
            .lock()
            .await
            .insert(first_placement, Arc::new(AtomicU64::new(0)));
        state
            .replica_loads
            .lock()
            .await
            .insert(second_placement, Arc::new(AtomicU64::new(10)));
        state
            .replica_loads
            .lock()
            .await
            .insert(third_placement, Arc::new(AtomicU64::new(5)));

        let selected = resolve_runtime(&state, &Bytes::from_static(b"{\"model\":\"qwen\"}"))
            .await
            .unwrap();
        assert_eq!(selected.runtime_url, ready_endpoint);
        assert!(selected.peer);
    }
}
