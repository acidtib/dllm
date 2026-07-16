use crate::{
    credentials::{
        CreatedCredential, CredentialError, CredentialRegistry, CredentialSummary, ManagementRole,
    },
    now_unix, NetworkStore, StoreError,
};
use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use dllm_protocol::{
    HardwareProfile, HealthState, ManagementStatus, NodeStatus, PlacementStatus, SignedJoinToken,
    WorkerStatus,
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
use tokio::sync::{Mutex, RwLock, Semaphore};

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<Mutex<NetworkStore>>,
    pub state_path: PathBuf,
    pub runtime_url: Option<String>,
    pub admission: Arc<Semaphore>,
    pub client: reqwest::Client,
    pub management_credentials: Arc<RwLock<CredentialRegistry>>,
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

#[derive(Debug, Deserialize)]
struct CreateCredentialRequest {
    label: String,
    role: ManagementRole,
}

pub fn router(state: ApiState) -> Router {
    let mut viewer = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/placements/preview", post(preview_placement));
    let mut operator = Router::new()
        .route("/v1/assignments", post(assign).delete(unassign))
        .route("/v1/hardware-profiles", post(publish_hardware_profile));
    let mut admin = Router::new()
        .route("/v1/invitations", post(invite))
        .route("/v1/members/revoke", post(revoke))
        .route(
            "/v1/management/credentials",
            get(list_credentials).post(create_credential),
        )
        .route(
            "/v1/management/credentials/{credential_id}",
            axum::routing::delete(revoke_credential),
        );
    let credentials = state.management_credentials.clone();
    if !credentials
        .try_read()
        .expect("credential registry is not locked during router construction")
        .is_empty()
    {
        viewer = viewer.route_layer(middleware::from_fn_with_state(
            (credentials.clone(), ManagementRole::Viewer),
            require_management_role,
        ));
        operator = operator.route_layer(middleware::from_fn_with_state(
            (credentials.clone(), ManagementRole::Operator),
            require_management_role,
        ));
        admin = admin.route_layer(middleware::from_fn_with_state(
            (credentials, ManagementRole::Admin),
            require_management_role,
        ));
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
        .merge(viewer)
        .merge(operator)
        .merge(admin)
        .merge(inference)
        .with_state(state)
}

pub fn multi_network_router(primary: ApiState, additional: Vec<(uuid::Uuid, ApiState)>) -> Router {
    additional
        .into_iter()
        .fold(router(primary), |app, (network_id, state)| {
            app.nest(&format!("/networks/{network_id}"), router(state))
        })
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

async fn require_management_role(
    State((credentials, required)): State<(Arc<RwLock<CredentialRegistry>>, ManagementRole)>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let Some(token) = supplied else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let Some(authorized) = credentials.read().await.authorize(&token, required) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !authorized {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(next.run(request).await)
}

async fn list_credentials(State(state): State<ApiState>) -> Json<Vec<CredentialSummary>> {
    Json(state.management_credentials.read().await.list())
}

async fn create_credential(
    State(state): State<ApiState>,
    Json(request): Json<CreateCredentialRequest>,
) -> Result<Json<CreatedCredential>, ApiError> {
    let created = state.management_credentials.write().await.create(
        request.label,
        request.role,
        now_unix(),
    )?;
    Ok(Json(created))
}

async fn revoke_credential(
    State(state): State<ApiState>,
    Path(credential_id): Path<uuid::Uuid>,
) -> Result<StatusCode, ApiError> {
    state
        .management_credentials
        .write()
        .await
        .revoke(credential_id)?;
    Ok(StatusCode::NO_CONTENT)
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

async fn publish_hardware_profile(
    State(state): State<ApiState>,
    Json(profile): Json<HardwareProfile>,
) -> Result<Json<MutationResponse>, ApiError> {
    let mut store = state.store.lock().await;
    let changed = store.publish_hardware_profile(profile)?;
    if changed {
        store.save(&state.state_path)?;
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

#[derive(Debug, Deserialize)]
struct PlacementPreviewRequest {
    model: String,
    architecture: String,
    required_memory_bytes: u64,
    compatible_backends: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PlacementPreview {
    model: String,
    generation: u64,
    candidates: Vec<PlacementCandidate>,
}

#[derive(Debug, Serialize)]
struct PlacementCandidate {
    node_pubkey: [u8; 32],
    compatible: bool,
    backend: Option<String>,
    memory_headroom_bytes: u64,
    decode_tokens_per_second_milli: Option<u64>,
    explanations: Vec<String>,
}

async fn preview_placement(
    State(state): State<ApiState>,
    Json(request): Json<PlacementPreviewRequest>,
) -> Json<PlacementPreview> {
    let store = state.store.lock().await;
    let mut candidates = store
        .state
        .state
        .hardware_profiles
        .iter()
        .map(|profile| preview_candidate(profile, &request))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .compatible
            .cmp(&left.compatible)
            .then_with(|| {
                right
                    .decode_tokens_per_second_milli
                    .cmp(&left.decode_tokens_per_second_milli)
            })
            .then_with(|| right.memory_headroom_bytes.cmp(&left.memory_headroom_bytes))
            .then_with(|| left.node_pubkey.cmp(&right.node_pubkey))
    });
    Json(PlacementPreview {
        model: request.model,
        generation: store.state.state.generation,
        candidates,
    })
}

fn preview_candidate(
    profile: &HardwareProfile,
    request: &PlacementPreviewRequest,
) -> PlacementCandidate {
    let runtime = profile.runtimes.iter().find(|runtime| {
        request.compatible_backends.contains(&runtime.backend)
            && runtime
                .architectures
                .iter()
                .any(|architecture| architecture == &request.architecture)
    });
    let enough_memory = profile.available_memory_bytes >= request.required_memory_bytes;
    let mut explanations = Vec::new();
    if runtime.is_none() {
        explanations.push(format!(
            "no runtime supports architecture {} on backends {}",
            request.architecture,
            request.compatible_backends.join(", ")
        ));
    }
    if !enough_memory {
        explanations.push(format!(
            "requires {} bytes but only {} bytes are available",
            request.required_memory_bytes, profile.available_memory_bytes
        ));
    }
    if runtime.is_some() && enough_memory {
        explanations.push("runtime and memory requirements satisfied".into());
    }
    let backend = runtime.map(|runtime| runtime.backend.clone());
    let decode_tokens_per_second_milli = backend.as_ref().and_then(|backend| {
        profile
            .benchmarks
            .iter()
            .filter(|benchmark| benchmark.model == request.model && &benchmark.backend == backend)
            .map(|benchmark| benchmark.decode_tokens_per_second_milli)
            .max()
    });
    PlacementCandidate {
        node_pubkey: profile.node_pubkey,
        compatible: runtime.is_some() && enough_memory,
        backend,
        memory_headroom_bytes: profile
            .available_memory_bytes
            .saturating_sub(request.required_memory_bytes),
        decode_tokens_per_second_milli,
        explanations,
    }
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
    Credential(CredentialError),
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<CredentialError> for ApiError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
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
            Self::Store(StoreError::ProfileNodeUnknown) => (
                StatusCode::BAD_REQUEST,
                "hardware profile node is not a network member",
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
            Self::Credential(CredentialError::EmptyLabel) => (
                StatusCode::BAD_REQUEST,
                "credential label must not be empty",
            )
                .into_response(),
            Self::Credential(CredentialError::PersistenceDisabled) => (
                StatusCode::CONFLICT,
                "credential persistence is not configured",
            )
                .into_response(),
            Self::Credential(CredentialError::NotRevocable) => (
                StatusCode::NOT_FOUND,
                "credential not found or not revocable",
            )
                .into_response(),
            Self::Credential(CredentialError::LastAdmin) => (
                StatusCode::CONFLICT,
                "the last admin credential cannot be revoked",
            )
                .into_response(),
            Self::Credential(error) => {
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use dllm_protocol::{
        AcceleratorCapability, CpuCapability, HardwareBenchmark, RuntimeCapability,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state(management_token: Option<&str>, runtime_url: Option<String>) -> ApiState {
        ApiState {
            store: Arc::new(Mutex::new(NetworkStore::create("test"))),
            state_path: std::env::temp_dir().join("dllmd-api-test-state.json"),
            runtime_url,
            admission: Arc::new(Semaphore::new(1)),
            client: reqwest::Client::new(),
            management_credentials: Arc::new(RwLock::new(
                CredentialRegistry::load(Vec::new(), management_token.map(str::to_owned), None)
                    .unwrap(),
            )),
            api_key: None,
            peer_api_key: None,
            metrics: Arc::new(Metrics::default()),
            public_url: "http://127.0.0.1:7337".into(),
            replica_loads: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn hardware_profile(node_pubkey: [u8; 32]) -> HardwareProfile {
        HardwareProfile {
            node_pubkey,
            observed_at_unix: 1,
            cpu: CpuCapability {
                model: "test cpu".into(),
                physical_cores: 4,
                logical_cores: 8,
                features: vec!["avx2".into()],
            },
            system_memory_bytes: 16_000_000_000,
            available_memory_bytes: 12_000_000_000,
            accelerators: vec![AcceleratorCapability {
                backend: "vulkan".into(),
                device_name: "test gpu".into(),
                device_id: "8086:5917".into(),
                driver: "mesa".into(),
                memory_bytes: None,
            }],
            runtimes: vec![RuntimeCapability {
                runtime: "llama.cpp".into(),
                revision: "505b1ed".into(),
                backend: "vulkan".into(),
                architectures: vec!["gemma3".into()],
            }],
            benchmarks: vec![HardwareBenchmark {
                model: "gemma".into(),
                backend: "vulkan".into(),
                context_size: 2048,
                concurrency: 1,
                prompt_tokens_per_second_milli: 10_000,
                decode_tokens_per_second_milli: 5_000,
                peak_memory_bytes: 2_000_000_000,
            }],
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
    async fn management_roles_enforce_least_privilege() {
        let mut api_state = state(None, None);
        api_state.management_credentials = Arc::new(RwLock::new(
            CredentialRegistry::load(
                vec![
                    crate::credentials::ManagementCredential {
                        token: "viewer-secret".into(),
                        role: ManagementRole::Viewer,
                    },
                    crate::credentials::ManagementCredential {
                        token: "operator-secret".into(),
                        role: ManagementRole::Operator,
                    },
                    crate::credentials::ManagementCredential {
                        token: "admin-secret".into(),
                        role: ManagementRole::Admin,
                    },
                ],
                None,
                None,
            )
            .unwrap(),
        ));
        let app = router(api_state);

        let viewer_status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, "Bearer viewer-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(viewer_status.status(), StatusCode::OK);

        let viewer_assignment = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/assignments")
                    .header(header::AUTHORIZATION, "Bearer viewer-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(viewer_assignment.status(), StatusCode::FORBIDDEN);

        let operator_invite = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/invitations")
                    .header(header::AUTHORIZATION, "Bearer operator-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"expires_at_unix":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(operator_invite.status(), StatusCode::FORBIDDEN);

        let admin_invite = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/invitations")
                    .header(header::AUTHORIZATION, "Bearer admin-secret")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"expires_at_unix":null}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admin_invite.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn management_credentials_rotate_without_exposing_secrets() {
        let path =
            std::env::temp_dir().join(format!("dllmd-credentials-{}.json", uuid::Uuid::new_v4()));
        let mut api_state = state(None, None);
        api_state.management_credentials = Arc::new(RwLock::new(
            CredentialRegistry::load(
                vec![crate::credentials::ManagementCredential {
                    token: "bootstrap-admin".into(),
                    role: ManagementRole::Admin,
                }],
                None,
                Some(path.clone()),
            )
            .unwrap(),
        ));
        let app = router(api_state);

        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/management/credentials")
                    .header(header::AUTHORIZATION, "Bearer bootstrap-admin")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"label":"monitoring","role":"viewer"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let created: serde_json::Value =
            serde_json::from_slice(&created.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let credential_id = created["credential"]["id"].as_str().unwrap();
        let token = created["token"].as_str().unwrap();

        let viewer_status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(viewer_status.status(), StatusCode::OK);

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/management/credentials")
                    .header(header::AUTHORIZATION, "Bearer bootstrap-admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = listed.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&listed).contains(token));
        assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains(token));

        let revoked = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/management/credentials/{credential_id}"))
                    .header(header::AUTHORIZATION, "Bearer bootstrap-admin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

        let revoked_status = app
            .oneshot(
                Request::builder()
                    .uri("/v1/status")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked_status.status(), StatusCode::UNAUTHORIZED);

        let reloaded = CredentialRegistry::load(
            Vec::new(),
            Some("bootstrap-admin".into()),
            Some(path.clone()),
        )
        .unwrap();
        assert_eq!(reloaded.authorize(token, ManagementRole::Viewer), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn model_listing_supports_multiple_models_and_deduplicates_replicas() {
        let state = state(None, None);
        let owner = state.store.lock().await.state.state.owner_pubkey;
        let member = NetworkStore::random_node_key();
        {
            let mut store = state.store.lock().await;
            let token = store.issue_join_token("http://owner".into(), None);
            store
                .redeem_join_token(token, member, "http://member".into())
                .unwrap();
            store.assign_model("qwen".into(), owner).unwrap();
            store.assign_model("qwen".into(), member).unwrap();
            store.assign_model("gemma".into(), owner).unwrap();
        }
        let response = router(state)
            .oneshot(Request::get("/v1/models").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let models = body["data"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["id"], "gemma");
        assert_eq!(models[1]["id"], "qwen");
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

    #[tokio::test]
    async fn hardware_profile_drives_read_only_placement_preview() {
        let state = state(None, None);
        let owner = state.store.lock().await.state.state.owner_pubkey;
        let app = router(state);
        let published = app
            .clone()
            .oneshot(
                Request::post("/v1/hardware-profiles")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&hardware_profile(owner)).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(published.status(), StatusCode::OK);
        let preview = app
            .clone()
            .oneshot(
                Request::post("/v1/placements/preview")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"model":"gemma","architecture":"gemma3","required_memory_bytes":2000000000,"compatible_backends":["vulkan","cpu"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preview.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&preview.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["generation"], 2);
        assert_eq!(body["candidates"][0]["compatible"], true);
        assert_eq!(body["candidates"][0]["backend"], "vulkan");
        assert_eq!(
            body["candidates"][0]["decode_tokens_per_second_milli"],
            5_000
        );
        let status = app
            .oneshot(Request::get("/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&status.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["network"]["state"]["generation"], 2);
    }

    #[tokio::test]
    async fn multiple_networks_keep_state_and_credentials_isolated() {
        let primary = state(Some("primary-secret"), None);
        let mut secondary = state(Some("secondary-secret"), None);
        secondary.store = Arc::new(Mutex::new(NetworkStore::create("secondary")));
        let secondary_id = secondary.store.lock().await.state.state.network_id;
        let app = multi_network_router(primary, vec![(secondary_id, secondary)]);

        let primary_status = app
            .clone()
            .oneshot(
                Request::get("/v1/status")
                    .header(header::AUTHORIZATION, "Bearer primary-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(primary_status.status(), StatusCode::OK);
        let wrong_credential = app
            .clone()
            .oneshot(
                Request::get(format!("/networks/{secondary_id}/v1/status"))
                    .header(header::AUTHORIZATION, "Bearer primary-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_credential.status(), StatusCode::UNAUTHORIZED);
        let secondary_status = app
            .oneshot(
                Request::get(format!("/networks/{secondary_id}/v1/status"))
                    .header(header::AUTHORIZATION, "Bearer secondary-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(secondary_status.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &secondary_status
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(body["network"]["state"]["name"], "secondary");
    }

    #[tokio::test]
    async fn bundled_ui_exposes_phase_two_orchestration_controls() {
        let response = router(state(None, None))
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Hardware capacity"));
        assert!(html.contains("Placement preview"));
        assert!(html.contains("replicas ready"));
        assert!(html.contains("networkMatch"));
    }
}
