use crate::{
    audit::AuditLog,
    budget::BudgetEnforcer,
    credentials::{
        CreatedCredential, CredentialError, CredentialRegistry, CredentialSummary, ManagementRole,
    },
    inference::{InferenceIdentity, InferencePolicy, InferenceRegistry},
    peer_service::PeerBundle,
    rate_limit::{RateLimitConfig, RateLimiter},
    NetworkStore, StoreError,
};
use axum::{
    body::{Body, Bytes},
    extract::{Extension, Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use dllm_protocol::{
    now_ms, now_unix, AccessRequest, HardwareProfile, HealthState, ManagementStatus, NodeStatus,
    PlacementLifecycle, PlacementStatus, ResourceBudget, SignedAccessRequest, SignedJoinToken,
    SignedStateFetchRequest, StateFetchRequest, StateFetchResponse, TransportKind, WorkerStatus,
};
use futures_util::{future::join_all, StreamExt};
use hmac::{Hmac, Mac};
use rust_embed::RustEmbed;
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
    pub runtime_url: Arc<RwLock<Option<String>>>,
    /// In-process inference runtime for local (owner) chat. When set, local
    /// inference is served by direct call instead of proxying to `runtime_url`.
    pub embedded: Arc<RwLock<Option<Arc<crate::embedded_runtime::EmbeddedRuntime>>>>,
    pub admission: Arc<Semaphore>,
    pub client: reqwest::Client,
    pub management_credentials: Arc<RwLock<CredentialRegistry>>,
    pub inference_credentials: Arc<InferenceRegistry>,
    pub peer_api_key: Option<String>,
    pub metrics: Arc<Metrics>,
    pub public_url: String,
    pub bootstrap_multiaddrs: Vec<String>,
    pub node_key_path: PathBuf,
    pub transport_key_path: PathBuf,
    pub config_path: PathBuf,
    pub authority_key_path: PathBuf,
    pub provisional_marker_path: PathBuf,
    pub onboarding: Arc<RwLock<OnboardingStatus>>,
    pub replica_loads: Arc<Mutex<HashMap<uuid::Uuid, Arc<AtomicU64>>>>,
    pub peer_nonces: Arc<Mutex<HashMap<String, u64>>>,
    pub peer_quota: Arc<Semaphore>,
    pub peer: Arc<RwLock<Option<PeerBundle>>>,
    pub peer_handle: Arc<Mutex<Option<dllm_transport::peer::PeerNodeHandle>>>,

    pub budget_enforcer: Arc<BudgetEnforcer>,
    pub rate_limiter: Arc<RateLimiter<String>>,
    pub access_request_rate_config: RateLimitConfig,
    pub audit_log: Option<Arc<AuditLog>>,
}

#[derive(Default)]
pub struct Metrics {
    inference_requests: AtomicU64,
    admission_rejections: AtomicU64,
    upstream_failures: AtomicU64,
    request_bytes: AtomicU64,
    response_bytes: AtomicU64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum OnboardingStatus {
    #[default]
    Inactive,
    Joining {
        authority_url: String,
        detail: String,
    },
    Active {
        authority_url: String,
    },
    Failed {
        authority_url: String,
        detail: String,
    },
}

#[derive(Debug, Deserialize)]
struct StartOnboardingRequest {
    authority_url: String,
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

#[derive(Debug, Deserialize)]
pub struct BindTransportRequest {
    pub node_pubkey: Vec<u8>,
    pub transport_peer_id: String,
    pub binding_generation: u64,
    pub expires_at_unix: u64,
}

#[derive(Debug, Deserialize)]
pub struct RevokeTransportRequest {
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct ForwardingPolicyRequest {
    pub node_pubkey: Vec<u8>,
    pub max_reservations: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct AccessRequestRequest {
    pub request: SignedAccessRequest,
}

#[derive(Debug, Deserialize)]
struct ApproveAccessRequest {
    pub node_pubkey: Vec<u8>,
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DenyAccessRequest {
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct SetBudgetRequest {
    pub node_pubkey: Vec<u8>,
    pub max_in_flight: u32,
    pub max_requests_per_window: u32,
    pub window_seconds: u32,
}

#[derive(Debug, Deserialize)]
struct RemoveBudgetRequest {
    pub node_pubkey: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct InferencePolicyResponse {
    credentials: Vec<InferencePolicy>,
    member_budgets: Vec<ResourceBudget>,
}

#[derive(Debug, Serialize)]
struct MutationResponse {
    generation: u64,
    changed: bool,
}

#[derive(Debug, Deserialize)]
struct BanNodeRequest {
    node_pubkey: Vec<u8>,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct UnbanNodeRequest {
    node_pubkey: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct SubmitAbuseReportRequest {
    report: dllm_protocol::AbuseReport,
}

#[derive(Debug, Deserialize)]
struct AuditLogQuery {
    since: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CreateCredentialRequest {
    label: String,
    role: ManagementRole,
}

pub fn router(state: ApiState) -> Router {
    let mut viewer = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/peer-network/status", get(peer_network_status))
        .route("/v1/inference-policy", get(inference_policy))
        .route("/v1/placements/preview", post(preview_placement))
        .route("/v1/access-requests", get(list_access_requests))
        .route("/v1/audit-log", get(audit_log));
    let mut operator = Router::new()
        .route("/v1/assignments", post(assign).delete(unassign))
        .route("/v1/hardware-profiles", post(publish_hardware_profile))
        .route(
            "/v1/placements/{placement_id}/drain",
            post(drain_placement).delete(resume_placement),
        );
    let mut admin = Router::new()
        .route("/v1/invitations", post(invite))
        .route("/v1/members/revoke", post(revoke))
        .route("/v1/transport-bindings", post(bind_transport))
        .route("/v1/transport-bindings/revoke", post(revoke_transport))
        .route("/v1/forwarding-policy", post(set_forwarding_policy))
        .route("/v1/access-requests/approve", post(approve_access_request))
        .route("/v1/access-requests/deny", post(deny_access_request))
        .route(
            "/v1/resource-budgets",
            post(set_resource_budget).delete(remove_resource_budget),
        )
        .route(
            "/v1/management/credentials",
            get(list_credentials).post(create_credential),
        )
        .route(
            "/v1/management/credentials/{credential_id}",
            axum::routing::delete(revoke_credential),
        )
        .route("/v1/moderation/bans", post(ban_node).delete(unban_node))
        .route("/v1/onboarding/start", post(start_onboarding))
        .route(
            "/v1/abuse-reports",
            get(list_abuse_reports).post(submit_abuse_report),
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
    inference = inference.route_layer(middleware::from_fn_with_state(
        state.inference_credentials.clone(),
        require_inference_identity,
    ));
    let mut peer = Router::new()
        .route("/v1/peer/health", get(|| async { StatusCode::OK }))
        .route("/v1/peer/health/runtime", get(runtime_health))
        .route("/v1/peer/chat/completions", post(proxy_peer_chat));
    if state.peer_api_key.is_some() {
        peer = peer.route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_peer_identity,
        ));
    }
    let rate_limit_state = state.clone();
    let access_request_route = Router::new()
        .route("/v1/access-requests", post(submit_access_request))
        .route_layer(middleware::from_fn_with_state(
            rate_limit_state,
            rate_limit_access_request,
        ));
    let mode_state = state.onboarding.clone();
    Router::new()
        .route("/", get(ui))
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/health/runtime", get(runtime_health))
        .route("/v1/onboarding/status", get(onboarding_status))
        .route("/metrics", get(metrics))
        .route("/v1/members/join", post(join))
        .route("/v1/state/fetch", post(fetch_state))
        .merge(access_request_route)
        .merge(viewer)
        .merge(operator)
        .merge(admin)
        .merge(inference)
        .merge(peer)
        .fallback(get(spa_fallback))
        .route_layer(middleware::from_fn_with_state(
            mode_state,
            require_active_mode,
        ))
        .with_state(state)
}

async fn require_active_mode(
    State(onboarding): State<Arc<RwLock<OnboardingStatus>>>,
    request: Request,
    next: Next,
) -> Response {
    let allowed = matches!(
        request.uri().path(),
        "/health" | "/v1/onboarding/status" | "/v1/onboarding/start"
    );
    if !allowed && matches!(*onboarding.read().await, OnboardingStatus::Joining { .. }) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "daemon is joining a network",
        )
            .into_response();
    }
    next.run(request).await
}

async fn onboarding_status(State(state): State<ApiState>) -> Json<OnboardingStatus> {
    Json(state.onboarding.read().await.clone())
}

async fn start_onboarding(
    State(state): State<ApiState>,
    Json(request): Json<StartOnboardingRequest>,
) -> Result<(StatusCode, Json<OnboardingStatus>), (StatusCode, String)> {
    start_onboarding_workflow(state, request.authority_url).await
}

pub async fn start_onboarding_workflow(
    state: ApiState,
    authority_url: String,
) -> Result<(StatusCode, Json<OnboardingStatus>), (StatusCode, String)> {
    let authority_url = authority_url.trim_end_matches('/').to_owned();
    let parsed_url = reqwest::Url::parse(&authority_url)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid authority URL".into()))?;
    let is_loopback_host = matches!(
        parsed_url.host_str(),
        Some("127.0.0.1") | Some("localhost") | Some("::1")
    );
    let allowed =
        parsed_url.scheme() == "https" || (parsed_url.scheme() == "http" && is_loopback_host);
    if !allowed {
        return Err((
            StatusCode::BAD_REQUEST,
            "remote onboarding requires HTTPS".into(),
        ));
    }
    {
        let current = state.onboarding.read().await;
        match &*current {
            OnboardingStatus::Joining {
                authority_url: active,
                ..
            }
            | OnboardingStatus::Active {
                authority_url: active,
            } if active == &authority_url => {
                return Ok((StatusCode::OK, Json(current.clone())));
            }
            OnboardingStatus::Joining { .. } => {
                return Err((
                    StatusCode::CONFLICT,
                    "onboarding is already running for another authority".into(),
                ));
            }
            OnboardingStatus::Active {
                authority_url: active,
            } => {
                return Err((
                    StatusCode::CONFLICT,
                    format!(
                        "already an active member of {active}; switching networks requires manual intervention"
                    ),
                ));
            }
            _ => {}
        }
    }
    {
        let store = state.store.lock().await;
        if store.owner_key.is_some() {
            let marker = std::fs::read(&state.provisional_marker_path).map_err(|_| {
                (StatusCode::CONFLICT, "this is an established authority network and cannot be abandoned automatically".into())
            })?;
            let marker: serde_json::Value = serde_json::from_slice(&marker).map_err(|_| {
                (
                    StatusCode::CONFLICT,
                    "the provisional network marker is invalid".into(),
                )
            })?;
            if !store.state.state.members.is_empty()
                || marker["generation"].as_u64() != Some(store.state.state.generation)
                || marker["authority_pubkey"] != serde_json::json!(store.state.state.owner_pubkey)
            {
                return Err((
                    StatusCode::CONFLICT,
                    "the provisional network has changed and must be migrated explicitly".into(),
                ));
            }
            let archive = state
                .state_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("archive")
                .join(format!("provisional-{}", now_unix()));
            std::fs::create_dir_all(&archive)
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            std::fs::copy(&state.state_path, archive.join("state.json"))
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            std::fs::copy(&state.authority_key_path, archive.join("authority.key"))
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            // The running peer transport (if any) is left alone here and only torn
            // down once run_onboarding actually confirms the new network, so a
            // failed join doesn't leave P2P permanently disabled.
        }
    }
    let joining = OnboardingStatus::Joining {
        authority_url: authority_url.clone(),
        detail: "submitting access request".into(),
    };
    *state.onboarding.write().await = joining.clone();
    let task_state = state.clone();
    tokio::spawn(async move {
        if let Err(detail) = run_onboarding(task_state.clone(), authority_url.clone()).await {
            *task_state.onboarding.write().await = OnboardingStatus::Failed {
                authority_url,
                detail,
            };
        }
    });
    Ok((StatusCode::ACCEPTED, Json(joining)))
}

async fn run_onboarding(state: ApiState, authority_url: String) -> Result<(), String> {
    let node_key =
        NetworkStore::load_owner_key(&state.node_key_path).map_err(|error| error.to_string())?;
    let transport_key = dllm_transport::peer::load_or_create_identity(&state.transport_key_path)
        .map_err(|error| error.to_string())?;
    let node_pubkey = node_key.verifying_key().to_bytes();
    let access = SignedAccessRequest::sign(
        AccessRequest {
            node_pubkey,
            requested_endpoint: state.public_url.clone(),
            note: "onboard".into(),
            requested_at_unix: now_unix(),
            transport_peer_id: Some(transport_key.public().to_peer_id().to_string()),
        },
        &node_key,
    );
    // Bounds both loops below so a bad authority_url, a denied request, or a
    // clock-skewed node fails into OnboardingStatus::Failed (which unblocks
    // require_active_mode) instead of retrying forever.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    let mut retry_seconds = 1;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out submitting access request after 10 minutes".into());
        }
        match state
            .client
            .post(format!("{authority_url}/v1/access-requests"))
            .json(&serde_json::json!({ "request": access }))
            .send()
            .await
        {
            Ok(response)
                if response.status().is_success() || response.status() == StatusCode::CONFLICT =>
            {
                break;
            }
            Ok(response) if response.status() == StatusCode::FORBIDDEN => {
                return Err(format!(
                    "access request rejected with {}",
                    response.status()
                ));
            }
            _ => {
                *state.onboarding.write().await = OnboardingStatus::Joining {
                    authority_url: authority_url.clone(),
                    detail: "retrying access request".into(),
                };
                tokio::time::sleep(Duration::from_secs(retry_seconds)).await;
                retry_seconds = (retry_seconds * 2).min(30);
            }
        }
    }
    *state.onboarding.write().await = OnboardingStatus::Joining {
        authority_url: authority_url.clone(),
        detail: "waiting for approval".into(),
    };
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(
                "timed out waiting for approval after 10 minutes; the request may still be \
                 pending or may have been declined, check with the network authority"
                    .into(),
            );
        }
        let fetch = SignedStateFetchRequest::sign(
            StateFetchRequest {
                node_pubkey,
                requested_at_unix: now_unix(),
            },
            &node_key,
        );
        if let Ok(response) = state
            .client
            .post(format!("{authority_url}/v1/state/fetch"))
            .json(&fetch)
            .send()
            .await
        {
            let status = response.status();
            if status.is_success() {
                let fetched: StateFetchResponse =
                    response.json().await.map_err(|error| error.to_string())?;
                fetched.state.verify().map_err(|error| error.to_string())?;
                if !fetched
                    .state
                    .state
                    .members
                    .iter()
                    .any(|member| member.node_pubkey == node_pubkey)
                {
                    return Err("fetched state does not include the local node".into());
                }
                for address in &fetched.bootstrap_multiaddrs {
                    address
                        .parse::<dllm_transport::peer::Multiaddr>()
                        .map_err(|error| error.to_string())?;
                }
                crate::local_config::update_p2p_bootstrap(
                    &state.config_path,
                    fetched.bootstrap_multiaddrs,
                )
                .map_err(|error| error.to_string())?;
                let replica = NetworkStore::from_signed_state(fetched.state)
                    .map_err(|error| error.to_string())?;
                // The fetched state verified and includes this node, so the join is
                // confirmed: safe to retire any previous peer transport now. The
                // state-file write below wakes spawn_peer_watcher, which starts a
                // fresh peer for the new network.
                if let Some(handle) = state.peer_handle.lock().await.take() {
                    handle.abort();
                }
                *state.peer.write().await = None;
                replica
                    .save(&state.state_path)
                    .map_err(|error| error.to_string())?;
                *state.store.lock().await = replica;
                if state.provisional_marker_path.exists() {
                    std::fs::remove_file(&state.provisional_marker_path)
                        .map_err(|error| error.to_string())?;
                }
                *state.onboarding.write().await = OnboardingStatus::Active { authority_url };
                return Ok(());
            }
            // A clock-skewed or otherwise invalid signature won't heal by
            // retrying, and a ban is a terminal answer -- both fail fast
            // instead of retrying until the deadline. "not a current member"
            // (still pending review) keeps retrying below.
            if status == StatusCode::UNAUTHORIZED {
                let detail = response.text().await.unwrap_or_default();
                return Err(format!(
                    "state fetch was rejected as unauthorized ({detail}); check that this \
                     node's clock is in sync with the authority"
                ));
            }
            if status == StatusCode::FORBIDDEN {
                let detail = response.text().await.unwrap_or_default();
                if detail.contains("banned") {
                    return Err(format!("access denied: {detail}"));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn fetch_state(
    State(state): State<ApiState>,
    Json(request): Json<SignedStateFetchRequest>,
) -> Result<Json<StateFetchResponse>, (StatusCode, &'static str)> {
    request
        .verify()
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid state fetch signature"))?;
    let now = now_unix();
    if request.request.requested_at_unix.abs_diff(now) > 60 {
        return Err((
            StatusCode::UNAUTHORIZED,
            "state fetch timestamp is outside the allowed window",
        ));
    }
    let store = state.store.lock().await;
    let node_pubkey = request.request.node_pubkey;
    if store
        .state
        .state
        .banned
        .iter()
        .any(|ban| ban.node_pubkey == node_pubkey)
    {
        return Err((StatusCode::FORBIDDEN, "node is banned"));
    }
    let member = node_pubkey == store.state.state.owner_pubkey
        || store
            .state
            .state
            .members
            .iter()
            .any(|member| member.node_pubkey == node_pubkey);
    if !member {
        return Err((StatusCode::FORBIDDEN, "node is not a current member"));
    }
    Ok(Json(StateFetchResponse {
        state: store.state.clone(),
        bootstrap_multiaddrs: state.bootstrap_multiaddrs.clone(),
    }))
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

async fn peer_network_status(
    State(state): State<ApiState>,
) -> Json<dllm_transport::peer::PeerDiagnostics> {
    Json(
        state
            .peer
            .read()
            .await
            .as_ref()
            .map(|bundle| bundle.diagnostics.borrow().clone())
            .unwrap_or_default(),
    )
}

async fn runtime_is_ready(state: &ApiState) -> bool {
    if state.embedded.read().await.is_some() {
        return true;
    }
    match state.runtime_url.read().await.clone() {
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

async fn require_inference_identity(
    State(registry): State<Arc<InferenceRegistry>>,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(identity) = registry.authenticate(supplied) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    request.extensions_mut().insert(identity);
    Ok(next.run(request).await)
}

async fn require_peer_identity(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if supplied != state.peer_api_key.as_deref() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let network_id = request
        .headers()
        .get("x-dllm-network-id")
        .and_then(|value| value.to_str().ok());
    let caller = request
        .headers()
        .get("x-dllm-node-key")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_hex_key);
    let timestamp = request
        .headers()
        .get("x-dllm-timestamp")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let nonce = request
        .headers()
        .get("x-dllm-nonce")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let signature = request
        .headers()
        .get("x-dllm-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let store = state.store.lock().await;
    let expected_network_id = store.state.state.network_id.to_string();
    if network_id != Some(expected_network_id.as_str()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let Some(caller) = caller else {
        return Err(StatusCode::FORBIDDEN);
    };
    if caller != store.state.state.owner_pubkey
        && !store
            .state
            .state
            .members
            .iter()
            .any(|member| member.node_pubkey == caller)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let (Some(timestamp), Some(nonce), Some(signature), Some(key)) =
        (timestamp, nonce, signature, state.peer_api_key.as_deref())
    else {
        return Err(StatusCode::FORBIDDEN);
    };
    let now = now_unix();
    if now.abs_diff(timestamp) > 30
        || !verify_peer_signature(
            key,
            &expected_network_id,
            &hex_key(&caller),
            timestamp,
            &nonce,
            request.method().as_str(),
            request.uri().path(),
            &signature,
        )
    {
        return Err(StatusCode::FORBIDDEN);
    }
    drop(store);
    let mut nonces = state.peer_nonces.lock().await;
    nonces.retain(|_, seen_at| now.saturating_sub(*seen_at) <= 60);
    if nonces.insert(nonce, now).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    drop(nonces);
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

async fn inference_policy(State(state): State<ApiState>) -> Json<InferencePolicyResponse> {
    let credentials = state.inference_credentials.policies();
    let member_budgets = state
        .store
        .lock()
        .await
        .state
        .state
        .resource_budgets
        .clone();
    Json(InferencePolicyResponse {
        credentials,
        member_budgets,
    })
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

#[derive(RustEmbed)]
#[folder = "../../apps/web/dist"]
struct WebAssets;

fn mime_for_path(path: &str) -> &'static str {
    match path.rfind('.').map(|i| &path[i + 1..]) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

async fn serve_embedded(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(file) = WebAssets::get(path) {
        return Response::builder()
            .header(header::CONTENT_TYPE, mime_for_path(path))
            .body(Body::from(file.data))
            .unwrap();
    }
    // SPA fallback: any non-file path serves index.html
    if let Some(html) = WebAssets::get("index.html") {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(html.data))
            .unwrap()
    } else {
        (StatusCode::NOT_FOUND, "Not found").into_response()
    }
}

async fn ui() -> impl IntoResponse {
    serve_embedded("index.html").await
}

async fn spa_fallback(req: Request) -> impl IntoResponse {
    serve_embedded(req.uri().path()).await
}

async fn status(State(state): State<ApiState>) -> Json<ManagementStatus> {
    let runtime_ready = runtime_is_ready(&state).await;
    let network = state.store.lock().await.state.clone();
    let mut nodes = vec![NodeStatus {
        node_pubkey: network.state.owner_pubkey,
        endpoint: state.public_url.clone(),
        owner: true,
        health: HealthState::Ready,
        transport: Some(TransportKind::Local),
    }];
    let probes = network.state.members.iter().map(|member| {
        let probe_state = state.clone();
        let member = member.clone();
        async move {
            let transport = resolve_member_transport(&probe_state, &member, false).await;
            NodeStatus {
                node_pubkey: member.node_pubkey,
                endpoint: transport
                    .as_ref()
                    .map_or_else(|| member.endpoint.clone(), |candidate| candidate.0.clone()),
                owner: false,
                health: if transport.is_some() {
                    HealthState::Ready
                } else {
                    HealthState::Unavailable
                },
                transport: transport.map(|candidate| candidate.1),
            }
        }
    });
    nodes.extend(join_all(probes).await);
    let runtime_probes = network.state.members.iter().map(|member| {
        let probe_state = state.clone();
        let member = member.clone();
        async move {
            let ready = resolve_member_transport(&probe_state, &member, true)
                .await
                .is_some();
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
            lifecycle: placement.lifecycle.clone(),
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
) -> Result<Json<SignedJoinToken>, ApiError> {
    Ok(Json(state.store.lock().await.try_issue_join_token(
        state.public_url.clone(),
        request.expires_at_unix,
    )?))
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

async fn submit_access_request(
    State(state): State<ApiState>,
    Json(body): Json<AccessRequestRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let mut store = state.store.lock().await;
    store.submit_access_request(body.request)?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn list_access_requests(
    State(state): State<ApiState>,
) -> Json<Vec<dllm_protocol::AccessRequest>> {
    let store = state.store.lock().await;
    Json(store.list_access_requests().to_vec())
}

async fn approve_access_request(
    State(state): State<ApiState>,
    Json(request): Json<ApproveAccessRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    // Use the requested_endpoint from the pending request as default.
    let endpoint = match request.endpoint {
        Some(ep) => ep,
        None => {
            let pending = store
                .list_access_requests()
                .iter()
                .find(|req| req.node_pubkey == node_pubkey)
                .ok_or(StoreError::AccessRequestNotFound)?;
            pending.requested_endpoint.clone()
        }
    };
    store.approve_access_request_with_transport(
        node_pubkey,
        endpoint,
        now_unix(),
        crate::AUTOMATIC_BINDING_LIFETIME_SECS,
    )?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn deny_access_request(
    State(state): State<ApiState>,
    Json(request): Json<DenyAccessRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    store.deny_access_request(node_pubkey)?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn rate_limit_access_request(
    State(state): State<ApiState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let now = now_unix();
    if !state
        .rate_limiter
        .check(&ip, now, &state.access_request_rate_config)
        .await
    {
        // Log the rate-limit rejection if audit is enabled.
        if let Some(ref audit) = state.audit_log {
            audit.log(crate::audit::AuditEntry {
                timestamp_unix: now,
                actor: ip,
                action: "access_request_rate_limited".into(),
                target: None,
                outcome: "rate_limit".into(),
            });
        }
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Ok(next.run(request).await)
}

async fn ban_node(
    State(state): State<ApiState>,
    Json(request): Json<BanNodeRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.ban_node(node_pubkey, request.reason.clone())?;
    if changed {
        store.save(&state.state_path)?;
    }
    if let Some(ref audit) = state.audit_log {
        audit.log(crate::audit::AuditEntry {
            timestamp_unix: now_unix(),
            actor: "admin".into(),
            action: "ban_node".into(),
            target: Some(hex::encode(node_pubkey)),
            outcome: if changed {
                "ok".into()
            } else {
                "unchanged".into()
            },
        });
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn unban_node(
    State(state): State<ApiState>,
    Json(request): Json<UnbanNodeRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.unban_node(node_pubkey)?;
    if changed {
        store.save(&state.state_path)?;
    }
    if let Some(ref audit) = state.audit_log {
        audit.log(crate::audit::AuditEntry {
            timestamp_unix: now_unix(),
            actor: "admin".into(),
            action: "unban_node".into(),
            target: Some(hex::encode(node_pubkey)),
            outcome: if changed { "ok".into() } else { "error".into() },
        });
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn submit_abuse_report(
    State(state): State<ApiState>,
    Json(request): Json<SubmitAbuseReportRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let mut store = state.store.lock().await;
    store.submit_abuse_report(request.report)?;
    store.save(&state.state_path)?;
    if let Some(ref audit) = state.audit_log {
        audit.log(crate::audit::AuditEntry {
            timestamp_unix: now_unix(),
            actor: "member".into(),
            action: "submit_abuse_report".into(),
            target: None,
            outcome: "ok".into(),
        });
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn list_abuse_reports(
    State(state): State<ApiState>,
) -> Json<Vec<dllm_protocol::AbuseReport>> {
    let store = state.store.lock().await;
    Json(store.list_abuse_reports().to_vec())
}

async fn audit_log(
    State(state): State<ApiState>,
    axum::extract::Query(query): axum::extract::Query<AuditLogQuery>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    let log_dir = state
        .state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("audit");
    let path = log_dir.join("audit.jsonl");
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Json(Vec::new()));
        }
        Err(e) => return Err(ApiError::Store(StoreError::Storage(e))),
    };
    let mut entries: Vec<serde_json::Value> = contents
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| {
            query.since.is_none_or(|since| {
                entry
                    .get("timestamp_unix")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|ts| ts >= since)
            })
        })
        .collect();
    if let Some(limit) = query.limit {
        entries.truncate(limit);
    }
    Ok(Json(entries))
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

async fn bind_transport(
    State(state): State<ApiState>,
    Json(request): Json<BindTransportRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    store.bind_transport_endpoint(
        node_pubkey,
        request.transport_peer_id,
        request.binding_generation,
        now_unix(),
        request.expires_at_unix,
    )?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn revoke_transport(
    State(state): State<ApiState>,
    Json(request): Json<RevokeTransportRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    store.revoke_transport_endpoint(node_pubkey, now_unix())?;
    store.save(&state.state_path)?;
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed: true,
    }))
}

async fn set_forwarding_policy(
    State(state): State<ApiState>,
    Json(request): Json<ForwardingPolicyRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.set_forwarding_policy(node_pubkey, request.max_reservations)?;
    if changed {
        store.save(&state.state_path)?;
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn set_resource_budget(
    State(state): State<ApiState>,
    Json(request): Json<SetBudgetRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.set_resource_budget(
        node_pubkey,
        request.max_in_flight,
        request.max_requests_per_window,
        request.window_seconds,
    )?;
    if changed {
        store.save(&state.state_path)?;
        // Reconcile the budget enforcer with the updated signed state.
        state.budget_enforcer.reconcile(&store.state.state).await;
    }
    Ok(Json(MutationResponse {
        generation: store.state.state.generation,
        changed,
    }))
}

async fn remove_resource_budget(
    State(state): State<ApiState>,
    Json(request): Json<RemoveBudgetRequest>,
) -> Result<Json<MutationResponse>, ApiError> {
    let node_pubkey = key_bytes(request.node_pubkey)?;
    let mut store = state.store.lock().await;
    let changed = store.remove_resource_budget(node_pubkey)?;
    if changed {
        store.save(&state.state_path)?;
        state.budget_enforcer.reconcile(&store.state.state).await;
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

async fn drain_placement(
    State(state): State<ApiState>,
    Path(placement_id): Path<uuid::Uuid>,
) -> Result<Json<MutationResponse>, ApiError> {
    set_placement_draining(state, placement_id, true).await
}

async fn resume_placement(
    State(state): State<ApiState>,
    Path(placement_id): Path<uuid::Uuid>,
) -> Result<Json<MutationResponse>, ApiError> {
    set_placement_draining(state, placement_id, false).await
}

async fn set_placement_draining(
    state: ApiState,
    placement_id: uuid::Uuid,
    draining: bool,
) -> Result<Json<MutationResponse>, ApiError> {
    let mut store = state.store.lock().await;
    let changed = store.set_placement_draining(placement_id, draining)?;
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
    Extension(identity): Extension<InferenceIdentity>,
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
        identity,
    )
    .await
}

async fn proxy_peer_chat(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let identity = InferenceIdentity {
        label: "peer".into(),
        quota: state.peer_quota.clone(),
    };
    let replica = resolve_runtime(&state, &body).await?;
    proxy(
        state,
        replica,
        reqwest::Method::POST,
        "/v1/chat/completions",
        headers,
        body,
        identity,
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
    identity: InferenceIdentity,
) -> Result<Response, ApiError> {
    state
        .metrics
        .inference_requests
        .fetch_add(1, Ordering::Relaxed);
    state
        .metrics
        .request_bytes
        .fetch_add(body.len() as u64, Ordering::Relaxed);
    let quota_permit = identity.quota.try_acquire_owned().map_err(|_| {
        state
            .metrics
            .admission_rejections
            .fetch_add(1, Ordering::Relaxed);
        ApiError::QuotaExceeded(identity.label)
    })?;
    let permit = state.admission.clone().try_acquire_owned().map_err(|_| {
        state
            .metrics
            .admission_rejections
            .fetch_add(1, Ordering::Relaxed);
        ApiError::Saturated
    })?;

    // Local owner served by the in-process embedded runtime: dispatch by direct
    // call instead of an HTTP hop. Only chat completions are routed here.
    if let Some(engine) = replica.embedded.clone() {
        return embedded_dispatch(state, engine, body, permit, quota_permit, replica.in_flight)
            .await;
    }

    // Use libp2p transport when a peer transport binding is available.
    let peer_available = state.peer.read().await.is_some();
    if let (Some(peer_id), true) = (replica.peer_id, peer_available) {
        return proxy_peer(
            state,
            peer_id,
            &method,
            path,
            &headers,
            &body,
            replica,
            permit,
            quota_permit,
        )
        .await;
    }

    let upstream_path = if replica.peer && state.peer_api_key.is_some() {
        format!("/v1/peer{}", path.strip_prefix("/v1").unwrap_or(path))
    } else {
        path.to_owned()
    };
    let upstream_method = method.as_str().to_owned();
    let mut request = state
        .client
        .request(method, format!("{}{upstream_path}", replica.runtime_url));
    if replica.peer {
        if let Some(key) = &state.peer_api_key {
            let store = state.store.lock().await;
            let network_id = store.state.state.network_id.to_string();
            let caller = hex_key(&store.state.state.owner_pubkey);
            drop(store);
            request = add_peer_identity(
                request,
                key,
                &network_id,
                &caller,
                &upstream_method,
                &upstream_path,
            );
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
        let _quota_permit = &quota_permit;
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

/// Serve a chat completion from the in-process embedded runtime, producing the
/// same JSON / SSE response the HTTP proxy would. Holds the admission and quota
/// permits and the replica lease for the lifetime of the response.
async fn embedded_dispatch(
    state: ApiState,
    engine: Arc<crate::embedded_runtime::EmbeddedRuntime>,
    body: Bytes,
    permit: tokio::sync::OwnedSemaphorePermit,
    quota_permit: tokio::sync::OwnedSemaphorePermit,
    in_flight: Arc<AtomicU64>,
) -> Result<Response, ApiError> {
    let request: serde_json::Value =
        serde_json::from_slice(&body).map_err(|_| ApiError::BadRequest("invalid JSON request"))?;
    let streaming = request
        .get("stream")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let lease = ReplicaLease::new(in_flight);
    let metrics = state.metrics.clone();

    if streaming {
        let rx = engine
            .chat_stream(request)
            .await
            .map_err(ApiError::Inference)?;
        let stream = futures_util::stream::unfold(
            (rx, metrics, permit, quota_permit, lease),
            |(mut rx, m, permit, quota_permit, lease)| async move {
                match rx.recv().await {
                    Some(chunk) => {
                        m.response_bytes
                            .fetch_add(chunk.len() as u64, Ordering::Relaxed);
                        Some((
                            Ok::<_, std::io::Error>(Bytes::from(chunk)),
                            (rx, m, permit, quota_permit, lease),
                        ))
                    }
                    None => None,
                }
            },
        );
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("X-Accel-Buffering", "no")
            .body(Body::from_stream(stream))
            .map_err(|_| ApiError::BadRequest("invalid embedded response"))
    } else {
        let value = engine
            .chat_blocking(request)
            .await
            .map_err(ApiError::Inference)?;
        let rendered = value.to_string();
        metrics
            .response_bytes
            .fetch_add(rendered.len() as u64, Ordering::Relaxed);
        drop((permit, quota_permit, lease));
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(rendered))
            .map_err(|_| ApiError::BadRequest("invalid embedded response"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn proxy_peer(
    state: ApiState,
    peer_id: dllm_transport::peer::PeerId,
    method: &reqwest::Method,
    path: &str,
    headers: &HeaderMap,
    body: &Bytes,
    replica: ResolvedReplica,
    _permit: tokio::sync::OwnedSemaphorePermit,
    _quota_permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<Response, ApiError> {
    let peer_client = state
        .peer
        .read()
        .await
        .as_ref()
        .map(|bundle| bundle.client.clone())
        .ok_or(ApiError::RuntimeUnavailable)?;

    let mut req_headers: HashMap<String, String> = HashMap::new();
    if let Some(ct) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        req_headers.insert("content-type".into(), ct.to_owned());
    }
    // Forward only safe headers: content-type and authorization.
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        req_headers.insert("authorization".into(), auth.to_owned());
    }

    let deadline_ms = now_ms() + 60_000;

    let peer_stream = match crate::peer_service::open_peer_inference(
        &peer_client,
        peer_id,
        method.as_str(),
        path,
        &req_headers,
        body,
        deadline_ms,
    )
    .await
    {
        Ok(stream) => stream,
        Err(_e) => {
            state
                .metrics
                .upstream_failures
                .fetch_add(1, Ordering::Relaxed);
            return Err(ApiError::BadRequest("peer inference failed"));
        }
    };

    let status = StatusCode::from_u16(peer_stream.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = peer_stream.content_type();
    let metrics = state.metrics.clone();
    let replica_lease = ReplicaLease::new(replica.in_flight);

    let body_stream = futures_util::stream::unfold(
        (peer_stream, metrics.clone(), replica_lease),
        |(mut ps, m, rl)| async move {
            let _ = &rl;
            match ps.read_chunk().await {
                Ok(Some(data)) => {
                    m.response_bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    Some((
                        Ok::<_, std::io::Error>(axum::body::Bytes::from(data)),
                        (ps, m, rl),
                    ))
                }
                Ok(None) => None,
                Err(e) => {
                    m.upstream_failures.fetch_add(1, Ordering::Relaxed);
                    Some((Err(std::io::Error::other(e)), (ps, m, rl)))
                }
            }
        },
    );

    let mut response = Response::builder().status(status);
    if let Some(ct) = content_type {
        response = response.header(header::CONTENT_TYPE, ct);
    }
    response
        .body(Body::from_stream(body_stream))
        .map_err(|_| ApiError::BadRequest("invalid upstream response"))
}

struct ResolvedReplica {
    runtime_url: String,
    peer: bool,
    in_flight: Arc<AtomicU64>,
    peer_id: Option<dllm_transport::peer::PeerId>,
    /// Set when this replica is the local owner served by the in-process
    /// embedded runtime; `proxy` then dispatches by direct call.
    embedded: Option<Arc<crate::embedded_runtime::EmbeddedRuntime>>,
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
                .filter(|placement| {
                    placement.model == model && placement.lifecycle == PlacementLifecycle::Ready
                })
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
        let (runtime_url, peer, ready, embedded) = if placement.node_pubkey == owner {
            // Prefer the in-process embedded runtime; fall back to an external
            // runtime_url (DLLMD_RUNTIME_URL) when present.
            if let Some(engine) = state.embedded.read().await.clone() {
                (String::new(), false, true, Some(engine))
            } else if let Some(runtime_url) = state.runtime_url.read().await.clone() {
                (runtime_url, false, runtime_is_ready(state).await, None)
            } else {
                // We are a replica without a local runtime. Route to the owner via libp2p.
                let peer_id = state
                    .peer
                    .read()
                    .await
                    .as_ref()
                    .and_then(|bundle| bundle.auth_view.resolve_peer(&owner));
                let transport = peer_id.map(|pid| (format!("peer://{pid}"), TransportKind::Direct));
                (
                    transport.as_ref().map_or_else(String::new, |c| c.0.clone()),
                    true,
                    transport.is_some(),
                    None,
                )
            }
        } else if let Some(member) = members
            .iter()
            .find(|member| member.node_pubkey == placement.node_pubkey)
        {
            let transport = resolve_member_transport(state, member, true).await;
            (
                transport
                    .as_ref()
                    .map_or_else(String::new, |candidate| candidate.0.clone()),
                true,
                transport.is_some(),
                None,
            )
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
            let peer_id = if peer {
                state
                    .peer
                    .read()
                    .await
                    .as_ref()
                    .and_then(|bundle| bundle.auth_view.resolve_peer(&placement.node_pubkey))
            } else {
                None
            };
            candidates.push((
                load.load(Ordering::Relaxed),
                placement.placement_id,
                runtime_url,
                peer,
                load,
                peer_id,
                embedded,
            ));
        }
    }
    candidates.sort_by_key(|(load, placement_id, ..)| (*load, *placement_id));
    candidates
        .into_iter()
        .next()
        .map(
            |(_, _, runtime_url, peer, in_flight, peer_id, embedded)| ResolvedReplica {
                runtime_url,
                peer,
                in_flight,
                peer_id,
                embedded,
            },
        )
        .ok_or(ApiError::RuntimeUnavailable)
}

async fn resolve_member_transport(
    state: &ApiState,
    member: &dllm_protocol::Member,
    runtime: bool,
) -> Option<(String, TransportKind)> {
    // Try peer health via libp2p first.
    let peer_client = state
        .peer
        .read()
        .await
        .as_ref()
        .map(|bundle| bundle.client.clone());
    if let Some(ref peer_client) = peer_client {
        if let Some(peer_id) = peer_client.auth().resolve_peer(&member.node_pubkey) {
            let _path = if runtime {
                "/v1/peer/health/runtime"
            } else {
                "/v1/peer/health"
            };
            // Use libp2p health check for reachability.
            if peer_client.health_check(peer_id).await.is_ok() {
                // Return a placeholder URL: the caller should use peer_id for routing.
                return Some((format!("peer://{peer_id}"), TransportKind::Direct));
            }
        }
    }

    let candidates = vec![(member.endpoint.clone(), TransportKind::Direct)];
    let (network_id, caller) = {
        let store = state.store.lock().await;
        (
            store.state.state.network_id.to_string(),
            hex_key(&store.state.state.owner_pubkey),
        )
    };
    for (endpoint, kind) in candidates {
        let path = match (state.peer_api_key.is_some(), runtime) {
            (true, true) => "/v1/peer/health/runtime",
            (true, false) => "/v1/peer/health",
            (false, true) => "/health/runtime",
            (false, false) => "/health",
        };
        let mut request = state
            .client
            .get(format!("{endpoint}{path}"))
            .timeout(Duration::from_secs(5));
        if let Some(key) = &state.peer_api_key {
            request = add_peer_identity(request, key, &network_id, &caller, "GET", path);
        }
        if request
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Some((endpoint, kind));
        }
    }
    None
}

fn hex_key(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hex_key(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn add_peer_identity(
    request: reqwest::RequestBuilder,
    key: &str,
    network_id: &str,
    caller: &str,
    method: &str,
    path: &str,
) -> reqwest::RequestBuilder {
    let timestamp = now_unix();
    let nonce = uuid::Uuid::new_v4().to_string();
    let signature = peer_signature(key, network_id, caller, timestamp, &nonce, method, path);
    request
        .bearer_auth(key)
        .header("x-dllm-network-id", network_id)
        .header("x-dllm-node-key", caller)
        .header("x-dllm-timestamp", timestamp)
        .header("x-dllm-nonce", nonce)
        .header("x-dllm-signature", signature)
}

fn peer_signature(
    key: &str,
    network_id: &str,
    caller: &str,
    timestamp: u64,
    nonce: &str,
    method: &str,
    path: &str,
) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(
        format!("{network_id}\n{caller}\n{timestamp}\n{nonce}\n{method}\n{path}").as_bytes(),
    );
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn verify_peer_signature(
    key: &str,
    network_id: &str,
    caller: &str,
    timestamp: u64,
    nonce: &str,
    method: &str,
    path: &str,
    signature: &str,
) -> bool {
    let Some(signature) = decode_hex(signature) else {
        return false;
    };
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(
        format!("{network_id}\n{caller}\n{timestamp}\n{nonce}\n{method}\n{path}").as_bytes(),
    );
    mac.verify_slice(&signature).is_ok()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
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
    /// Failure from the in-process embedded runtime.
    Inference(dllm_inference::InferenceError),
    ModelUnavailable,
    QuotaExceeded(String),
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
            Self::Inference(error) => {
                let status = if error.is_invalid() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (status, error.to_string()).into_response()
            }
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
            Self::Store(
                StoreError::BindingNodeUnknown
                | StoreError::InvalidBindingLifetime
                | StoreError::InvalidTransportPeerId
                | StoreError::TransportPeerIdInUse
                | StoreError::ForwardingNodeUnknown
                | StoreError::State(dllm_protocol::StateError::InvalidTransportPeerId),
            ) => (StatusCode::BAD_REQUEST, "transport binding rejected").into_response(),
            Self::Store(StoreError::StaleBindingGeneration { .. }) => (
                StatusCode::CONFLICT,
                "transport binding generation is stale",
            )
                .into_response(),
            Self::Store(
                StoreError::AccessRequestAlreadyPending | StoreError::AccessRequestAlreadyMember,
            ) => (StatusCode::CONFLICT, "access request already exists").into_response(),
            Self::Store(StoreError::BindingNotFound) => {
                (StatusCode::NOT_FOUND, "active transport binding not found").into_response()
            }
            Self::Store(StoreError::OwnerAuthorityUnavailable) => (
                StatusCode::FORBIDDEN,
                "this node does not hold network authority",
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
            Self::QuotaExceeded(label) => (
                StatusCode::TOO_MANY_REQUESTS,
                format!("inference quota exceeded for credential {label}"),
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

    const PEER_A: &str = "12D3KooWSahP5pFRCEfaziPEba7urXGeif6T1y8jmodzdFUvzBHj";
    const PEER_B: &str = "12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY";

    fn state(management_token: Option<&str>, runtime_url: Option<String>) -> ApiState {
        ApiState {
            store: Arc::new(Mutex::new(NetworkStore::create("test"))),
            state_path: std::env::temp_dir().join("dllmd-api-test-state.json"),
            runtime_url: Arc::new(RwLock::new(runtime_url)),
            embedded: Arc::new(RwLock::new(None)),
            admission: Arc::new(Semaphore::new(1)),
            client: reqwest::Client::new(),
            management_credentials: Arc::new(RwLock::new(
                CredentialRegistry::load(Vec::new(), management_token.map(str::to_owned), None)
                    .unwrap(),
            )),
            inference_credentials: Arc::new(InferenceRegistry::new(Vec::new(), None, 1)),
            peer_api_key: None,
            metrics: Arc::new(Metrics::default()),
            public_url: "http://127.0.0.1:7337".into(),
            bootstrap_multiaddrs: Vec::new(),
            node_key_path: std::env::temp_dir().join("dllmd-test-node.key"),
            transport_key_path: std::env::temp_dir().join("dllmd-test-transport.key"),
            config_path: std::env::temp_dir().join("dllmd-test-config.json"),
            authority_key_path: std::env::temp_dir().join("dllmd-test-authority.key"),
            provisional_marker_path: std::env::temp_dir().join("dllmd-test-provisional.json"),
            onboarding: Arc::new(RwLock::new(OnboardingStatus::Inactive)),
            replica_loads: Arc::new(Mutex::new(HashMap::new())),
            peer_nonces: Arc::new(Mutex::new(HashMap::new())),
            peer_quota: Arc::new(Semaphore::new(1)),
            peer: Arc::new(RwLock::new(None)),
            peer_handle: Arc::new(Mutex::new(None)),
            budget_enforcer: Arc::new(crate::budget::BudgetEnforcer::new()),
            rate_limiter: Arc::new(crate::rate_limit::RateLimiter::new()),
            access_request_rate_config: crate::rate_limit::RateLimitConfig::default(),
            audit_log: None,
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
                gpu_layers: 18,
                context_size: 2048,
                concurrency: 1,
                prompt_tokens_per_second_milli: 10_000,
                decode_tokens_per_second_milli: 5_000,
                peak_memory_bytes: 2_000_000_000,
            }],
        }
    }

    #[tokio::test]
    async fn state_fetch_requires_a_signed_current_member() {
        let mut state = state(None, None);
        state.bootstrap_multiaddrs = vec![format!("/ip4/127.0.0.1/tcp/7444/p2p/{PEER_A}")];
        let member_key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
        let member = member_key.verifying_key().to_bytes();
        {
            let mut store = state.store.lock().await;
            let token = store.issue_join_token("http://authority".into(), None);
            store
                .redeem_join_token(token, member, "http://member".into())
                .unwrap();
        }
        let signed = SignedStateFetchRequest::sign(
            StateFetchRequest {
                node_pubkey: member,
                requested_at_unix: now_unix(),
            },
            &member_key,
        );
        let response = router(state.clone())
            .oneshot(
                Request::post("/v1/state/fetch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&signed).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let fetched: StateFetchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(fetched.bootstrap_multiaddrs, state.bootstrap_multiaddrs);

        let unknown_key = ed25519_dalek::SigningKey::generate(&mut rand::thread_rng());
        let unknown = SignedStateFetchRequest::sign(
            StateFetchRequest {
                node_pubkey: unknown_key.verifying_key().to_bytes(),
                requested_at_unix: now_unix(),
            },
            &unknown_key,
        );
        let response = router(state)
            .oneshot(
                Request::post("/v1/state/fetch")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&unknown).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
    async fn transport_binding_api_persists_rotation_and_rejects_replay() {
        let api_state = state(Some("secret"), None);
        let store = api_state.store.clone();
        let owner = store.lock().await.state.state.owner_pubkey;
        let app = router(api_state);
        let request = |peer: &str, generation: u64| {
            Request::builder()
                .method("POST")
                .uri("/v1/transport-bindings")
                .header(header::AUTHORIZATION, "Bearer secret")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "node_pubkey": owner,
                        "transport_peer_id": peer,
                        "binding_generation": generation,
                        "expires_at_unix": u64::MAX
                    }))
                    .unwrap(),
                ))
                .unwrap()
        };

        assert_eq!(
            app.clone()
                .oneshot(request(PEER_A, 1))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(request(PEER_B, 2))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(request(PEER_A, 1))
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert!(store
            .lock()
            .await
            .authorize_transport_endpoint(owner, PEER_B, now_unix())
            .is_ok());

        let revoke = Request::builder()
            .method("POST")
            .uri("/v1/transport-bindings/revoke")
            .header(header::AUTHORIZATION, "Bearer secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "node_pubkey": owner })).unwrap(),
            ))
            .unwrap();
        assert_eq!(app.oneshot(revoke).await.unwrap().status(), StatusCode::OK);
        let store = store.lock().await;
        assert!(store.state.state.transport_bindings.is_empty());
        assert_eq!(store.state.state.transport_revocations.len(), 2);
        assert!(store.state.verify().is_ok());
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
        state.inference_credentials = Arc::new(InferenceRegistry::new(
            Vec::new(),
            Some("inference-secret".into()),
            1,
        ));
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
    async fn inference_quota_isolated_from_other_credentials() {
        let upstream = Router::new()
            .route("/health", get(|| async { StatusCode::OK }))
            .route(
                "/v1/chat/completions",
                post(|| async { Json(serde_json::json!({"choices": []})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let mut state = state(None, Some(format!("http://{address}")));
        let registry = Arc::new(InferenceRegistry::new(
            vec![
                crate::inference::InferenceCredential {
                    label: "client-a".into(),
                    token: "token-a".into(),
                    max_in_flight: 1,
                },
                crate::inference::InferenceCredential {
                    label: "client-b".into(),
                    token: "token-b".into(),
                    max_in_flight: 1,
                },
            ],
            None,
            2,
        ));
        let client_a = registry.authenticate(Some("token-a")).unwrap();
        let held = client_a.quota.acquire_owned().await.unwrap();
        state.inference_credentials = registry;
        state.admission = Arc::new(Semaphore::new(2));
        let owner = state.store.lock().await.state.state.owner_pubkey;
        state
            .store
            .lock()
            .await
            .assign_model("qwen".into(), owner)
            .unwrap();
        let app = router(state);

        let limited = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer token-a")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"model\":\"qwen\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        let body = limited.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("client-a"));

        let other_client = app
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer token-b")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"model\":\"qwen\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_client.status(), StatusCode::OK);
        drop(held);
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
            .route(
                "/v1/peer/health/runtime",
                get(|headers: HeaderMap| async move {
                    if headers.get("x-dllm-network-id").is_none()
                        || headers.get("x-dllm-node-key").is_none()
                    {
                        return StatusCode::FORBIDDEN;
                    }
                    StatusCode::OK
                }),
            )
            .route(
                "/v1/peer/chat/completions",
                post(|headers: HeaderMap| async move {
                    if headers
                        .get(header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        != Some("Bearer peer-secret")
                        || headers.get("x-dllm-network-id").is_none()
                        || headers.get("x-dllm-node-key").is_none()
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
    async fn peer_identity_is_required_and_direct_failure_does_not_use_legacy_relay() {
        let mut api_state = state(None, None);
        api_state.peer_api_key = Some("peer-secret".into());
        let member_key = NetworkStore::random_node_key();
        let member = {
            let mut store = api_state.store.lock().await;
            let token = store.issue_join_token("http://owner".into(), None);
            store
                .redeem_join_token(token, member_key, "http://127.0.0.1:9".into())
                .unwrap();
            store.state.state.members[0].clone()
        };
        assert!(resolve_member_transport(&api_state, &member, true)
            .await
            .is_none());

        let network = api_state.store.lock().await.state.state.clone();
        let app = router(api_state);
        let missing_identity = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/peer/health")
                    .header(header::AUTHORIZATION, "Bearer peer-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing_identity.status(), StatusCode::FORBIDDEN);

        let timestamp = now_unix();
        let nonce = "fixed-replay-nonce";
        let network_id = network.network_id.to_string();
        let caller = hex_key(&network.owner_pubkey);
        let signature = peer_signature(
            "peer-secret",
            &network_id,
            &caller,
            timestamp,
            nonce,
            "GET",
            "/v1/peer/health",
        );
        let peer_request = || {
            Request::builder()
                .uri("/v1/peer/health")
                .header(header::AUTHORIZATION, "Bearer peer-secret")
                .header("x-dllm-network-id", &network_id)
                .header("x-dllm-node-key", &caller)
                .header("x-dllm-timestamp", timestamp)
                .header("x-dllm-nonce", nonce)
                .header("x-dllm-signature", &signature)
                .body(Body::empty())
                .unwrap()
        };
        let valid_identity = app.clone().oneshot(peer_request()).await.unwrap();
        assert_eq!(valid_identity.status(), StatusCode::OK);
        let replay = app.oneshot(peer_request()).await.unwrap();
        assert_eq!(replay.status(), StatusCode::CONFLICT);
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
                .redeem_join_token(second_token, second, busy_endpoint.clone())
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

        state
            .store
            .lock()
            .await
            .set_placement_draining(third_placement, true)
            .unwrap();
        let after_drain = resolve_runtime(&state, &Bytes::from_static(b"{\"model\":\"qwen\"}"))
            .await
            .unwrap();
        assert_eq!(after_drain.runtime_url, busy_endpoint);
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
    async fn bundled_ui_exposes_phase_three_management_controls() {
        let response = router(state(None, None))
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/html"));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<title>DLLM</title>"));
        // SPA fallback serves index.html for unknown paths too
        let fallback = router(state(None, None))
            .oneshot(Request::get("/nodes").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(fallback.status(), StatusCode::OK);
        let fallback_body = fallback.into_body().collect().await.unwrap().to_bytes();
        let fallback_html = String::from_utf8(fallback_body.to_vec()).unwrap();
        assert!(fallback_html.contains("<title>DLLM</title>"));
    }
}
