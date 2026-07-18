use axum_server::{tls_rustls::RustlsConfig, Handle};
use dllm_daemon::{
    api,
    audit::AuditLog,
    budget::BudgetEnforcer,
    credentials::{CredentialRegistry, ManagementCredential},
    inference::{InferenceCredential, InferenceRegistry},
    rate_limit::{RateLimitConfig, RateLimiter},
    NetworkStore, StoreError,
};
use dllm_protocol::now_unix;
use dllm_runtime::{BundledModelSource, BundledRuntimeConfig, LlamaCppConfig, RuntimeWorker};
use dllm_transport::peer::{
    load_or_create_identity, start_peer_node, DiscoveryMode, Multiaddr, PeerId, PeerNodeConfig,
    PeerNodeHandle,
};
use serde::Deserialize;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Semaphore},
};

/// Lifetime for the owner's self-signed transport binding created at
/// bootstrap. Long enough to never expire in practice; the owner can
/// revoke or rebind at any time via `dllm bind-transport --owner` /
/// `dllm revoke-transport --owner`.
const OWNER_SELF_BINDING_LIFETIME_SECS: u64 = 100 * 365 * 24 * 60 * 60;

#[derive(Deserialize)]
struct AdditionalNetworkConfig {
    name: String,
    state_path: PathBuf,
    owner_key_path: PathBuf,
    management_token: Option<String>,
    #[serde(default)]
    management_credentials: Vec<ManagementCredential>,
    management_credentials_path: Option<PathBuf>,
    api_key: String,
    #[serde(default)]
    inference_credentials: Vec<InferenceCredential>,
    public_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bind = std::env::var("DLLMD_BIND").unwrap_or_else(|_| "127.0.0.1:7337".into());
    let state_path = match std::env::var("DLLMD_STATE") {
        Ok(value) => PathBuf::from(value),
        Err(_) => dllm_daemon::default_state_path()?,
    };
    let owner_key_path = match std::env::var("DLLMD_OWNER_KEY") {
        Ok(value) => PathBuf::from(value),
        Err(_) => dllm_daemon::default_owner_key_path()?,
    };
    let network_name = std::env::var("DLLMD_NETWORK").unwrap_or_else(|_| "private".into());
    let mut runtime_url = std::env::var("DLLMD_RUNTIME_URL").ok();
    let admission_limit = std::env::var("DLLMD_ADMISSION_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let (management_token, management_token_generated) =
        dllm_daemon::local_config::resolve_management_token()?;
    if management_token_generated {
        println!("generated management token: {management_token}");
    }
    let management_token = Some(management_token);
    let management_credentials = std::env::var("DLLMD_MANAGEMENT_CREDENTIALS")
        .ok()
        .map(|value| serde_json::from_str::<Vec<ManagementCredential>>(&value))
        .transpose()?
        .unwrap_or_default();
    let management_credentials_path = std::env::var("DLLMD_MANAGEMENT_CREDENTIALS_PATH")
        .ok()
        .map(PathBuf::from);
    let (api_key, api_key_generated) = dllm_daemon::local_config::resolve_api_key()?;
    if api_key_generated {
        println!("generated api key: {api_key}");
    }
    let api_key = Some(api_key);
    let inference_credentials = std::env::var("DLLMD_INFERENCE_CREDENTIALS")
        .ok()
        .map(|value| serde_json::from_str::<Vec<InferenceCredential>>(&value))
        .transpose()?
        .unwrap_or_default();
    let peer_api_key = std::env::var("DLLMD_PEER_API_KEY").ok();
    let tls_cert = std::env::var("DLLMD_TLS_CERT").ok();
    let tls_key = std::env::var("DLLMD_TLS_KEY").ok();
    let public_url = std::env::var("DLLMD_PUBLIC_URL").unwrap_or_else(|_| format!("http://{bind}"));
    let bind_address: SocketAddr = bind.parse()?;
    // management_token and api_key are always Some here (local_config generates them
    // when unset), so the two credential conditions below are effectively always
    // satisfied and this guard now only enforces the TLS cert/key presence for
    // non-loopback binds.
    if !bind_address.ip().is_loopback()
        && (!has_management_access(&management_token, &management_credentials)
            || (api_key.is_none() && inference_credentials.is_empty())
            || tls_cert.is_none()
            || tls_key.is_none())
    {
        return Err(
            "non-loopback binds require management and inference credentials plus TLS certificate and key"
                .into(),
        );
    }
    let store = if state_path.exists() {
        if owner_key_path.exists() {
            NetworkStore::load(&state_path, &owner_key_path)?
        } else {
            NetworkStore::load_replica(&state_path)?
        }
    } else {
        let mut store = NetworkStore::create(network_name);
        store.save_owner_key(&owner_key_path)?;
        if env_bool("DLLMD_P2P_ENABLED", false)? {
            let transport_key_path = resolve_transport_key_path()?;
            let transport_key = load_or_create_identity(&transport_key_path)?;
            let local_peer = transport_key.public().to_peer_id();
            let owner_pubkey = store.state.state.owner_pubkey;
            let issued_at = now_unix();
            let binding_generation = store.next_binding_generation(owner_pubkey);
            store.bind_transport_endpoint(
                owner_pubkey,
                local_peer.to_string(),
                binding_generation,
                issued_at,
                issued_at + OWNER_SELF_BINDING_LIFETIME_SECS,
            )?;
        }
        store.save(&state_path)?;
        store
    };
    let p2p_requested = env_bool("DLLMD_P2P_ENABLED", false)?;
    let peer_bundle: Arc<tokio::sync::RwLock<Option<dllm_daemon::peer_service::PeerBundle>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    let mut peer_handle: Option<PeerNodeHandle> = None;
    if let Some((handle, bundle)) = try_start_peer(&store, &owner_key_path, admission_limit)? {
        peer_handle = Some(handle);
        *peer_bundle.write().await = Some(bundle);
    }

    let mut runtime_worker = None;
    if runtime_url.is_none() {
        if let Ok(binary) = std::env::var("DLLMD_RUNTIME_BIN") {
            // DLLMD_RUNTIME_BIN is an explicit request for an external runtime.
            // Only start it if a model is also configured; never fall through
            // to the bundled binary here, even if misconfigured.
            if let Ok(model) = std::env::var("DLLMD_MODEL_PATH") {
                let config = LlamaCppConfig {
                    binary: binary.into(),
                    model: model.into(),
                    host: "127.0.0.1".into(),
                    port: std::env::var("DLLMD_RUNTIME_PORT")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(8081),
                    gpu_layers: std::env::var("DLLMD_GPU_LAYERS").unwrap_or_else(|_| "38".into()),
                    context_size: std::env::var("DLLMD_CONTEXT_SIZE")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(2048),
                    extra_args: vec![],
                };
                let worker = RuntimeWorker::start(&config, Duration::from_secs(300)).await?;
                runtime_url = Some(worker.endpoint().to_owned());
                runtime_worker = Some(worker);
            }
        } else {
            let model_path = std::env::var("DLLMD_MODEL_PATH").ok();
            let hf_model = std::env::var("DLLMD_HF_MODEL").ok();
            let model_source = match (model_path, hf_model) {
                (Some(_), Some(_)) => {
                    return Err("DLLMD_MODEL_PATH and DLLMD_HF_MODEL are mutually exclusive".into());
                }
                (Some(path), None) => Some(BundledModelSource::Local(path.into())),
                (None, Some(repo)) => Some(BundledModelSource::HuggingFace(repo)),
                (None, None) => None,
            };
            if let Some(model) = model_source {
                let config = BundledRuntimeConfig {
                    binary: bundled_runtime_binary()?,
                    model,
                    host: "127.0.0.1".into(),
                    port: std::env::var("DLLMD_RUNTIME_PORT")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(8081),
                    gpu_layers: std::env::var("DLLMD_GPU_LAYERS")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(38),
                    context_size: std::env::var("DLLMD_CONTEXT_SIZE")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(2048)
                        .into(),
                    api_key: None,
                    parallel: 1,
                    mmproj: std::env::var("DLLMD_MMPROJ_PATH").ok().map(PathBuf::from),
                };
                let worker =
                    RuntimeWorker::start_bundled(&config, Duration::from_secs(300)).await?;
                runtime_url = Some(worker.endpoint().to_owned());
                runtime_worker = Some(worker);
            }
        }
    }
    let primary_state = api::ApiState {
        store: Arc::new(Mutex::new(store)),
        state_path: state_path.clone(),
        runtime_url: runtime_url.clone(),
        admission: Arc::new(Semaphore::new(admission_limit)),
        client: reqwest::Client::new(),
        management_credentials: Arc::new(tokio::sync::RwLock::new(CredentialRegistry::load(
            management_credentials,
            management_token.clone(),
            management_credentials_path,
        )?)),
        inference_credentials: Arc::new(InferenceRegistry::new(
            inference_credentials,
            api_key.clone(),
            admission_limit,
        )),
        peer_api_key: peer_api_key.clone(),
        metrics: Arc::new(api::Metrics::default()),
        public_url: public_url.clone(),
        replica_loads: Arc::new(Mutex::new(HashMap::new())),
        peer_nonces: Arc::new(Mutex::new(HashMap::new())),
        peer_quota: Arc::new(Semaphore::new(admission_limit)),
        peer: peer_bundle.clone(),
        budget_enforcer: Arc::new(BudgetEnforcer::new()),
        rate_limiter: Arc::new(RateLimiter::new()),
        access_request_rate_config: RateLimitConfig {
            max_requests: std::env::var("DLLMD_ACCESS_REQUEST_RATE_LIMIT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            window_seconds: std::env::var("DLLMD_ACCESS_REQUEST_RATE_WINDOW")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
        },
        audit_log: Some(Arc::new(AuditLog::new(
            state_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("audit"),
            10 * 1024 * 1024, // 10 MB rotation threshold
        ))),
    };
    let additional_configs = std::env::var("DLLMD_ADDITIONAL_NETWORKS")
        .ok()
        .map(|value| serde_json::from_str::<Vec<AdditionalNetworkConfig>>(&value))
        .transpose()?
        .unwrap_or_default();
    let mut additional = Vec::with_capacity(additional_configs.len());
    for config in additional_configs {
        if !bind_address.ip().is_loopback()
            && !has_management_access(&config.management_token, &config.management_credentials)
        {
            return Err(
                "each additional network requires a management credential on a non-loopback bind"
                    .into(),
            );
        }
        let store = if config.state_path.exists() {
            if config.owner_key_path.exists() {
                NetworkStore::load(&config.state_path, &config.owner_key_path)?
            } else {
                NetworkStore::load_replica(&config.state_path)?
            }
        } else {
            let store = NetworkStore::create(config.name);
            store.save_owner_key(&config.owner_key_path)?;
            store.save(&config.state_path)?;
            store
        };
        let network_id = store.state.state.network_id;
        let network_public_url = config
            .public_url
            .unwrap_or_else(|| format!("{public_url}/networks/{network_id}"));
        let additional_state_path = config.state_path.clone();
        additional.push((
            network_id,
            api::ApiState {
                store: Arc::new(Mutex::new(store)),
                state_path: config.state_path,
                runtime_url: runtime_url.clone(),
                admission: Arc::new(Semaphore::new(admission_limit)),
                client: reqwest::Client::new(),
                management_credentials: Arc::new(tokio::sync::RwLock::new(
                    CredentialRegistry::load(
                        config.management_credentials,
                        config.management_token,
                        config.management_credentials_path,
                    )?,
                )),
                inference_credentials: Arc::new(InferenceRegistry::new(
                    config.inference_credentials,
                    Some(config.api_key),
                    admission_limit,
                )),
                peer_api_key: peer_api_key.clone(),
                metrics: Arc::new(api::Metrics::default()),
                public_url: network_public_url,
                replica_loads: Arc::new(Mutex::new(HashMap::new())),
                peer_nonces: Arc::new(Mutex::new(HashMap::new())),
                peer_quota: Arc::new(Semaphore::new(admission_limit)),
                peer: Arc::new(tokio::sync::RwLock::new(None)),
                budget_enforcer: Arc::new(BudgetEnforcer::new()),
                rate_limiter: Arc::new(RateLimiter::new()),
                access_request_rate_config: RateLimitConfig::default(),
                audit_log: Some(Arc::new(AuditLog::new(
                    additional_state_path
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join("audit"),
                    10 * 1024 * 1024,
                ))),
            },
        ));
    }
    if let Some(ref bundle) = *peer_bundle.read().await {
        let _dispatcher = dllm_daemon::peer_service::spawn_dispatcher(
            bundle.client.clone(),
            primary_state.clone(),
        );
    }
    if p2p_requested && peer_handle.is_none() {
        spawn_peer_watcher(
            state_path.clone(),
            owner_key_path.clone(),
            admission_limit,
            peer_bundle.clone(),
            primary_state.clone(),
        );
    }

    let app = api::multi_network_router(primary_state, additional);
    println!("dllmd listening on {bind}");
    if let (Some(cert), Some(key)) = (tls_cert, tls_key) {
        let config = RustlsConfig::from_pem_file(cert, key).await?;
        let handle = Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            shutdown().await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(10)));
        });
        axum_server::bind_rustls(bind_address, config)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await?;
    } else {
        let listener = TcpListener::bind(bind_address).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown())
        .await?;
    }
    if let Some(worker) = runtime_worker {
        worker.shutdown().await?;
    }
    if let Some(peer) = peer_handle {
        peer.abort();
    }
    Ok(())
}

fn bundled_runtime_binary() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let current_exe = std::env::current_exe()?;
    let dir = current_exe
        .parent()
        .ok_or("could not determine directory of current executable")?;
    let candidate = dir.join("dllm-llama-server");
    if !candidate.exists() {
        return Err(format!(
            "bundled runtime binary not found at {}; build it with `cargo build --release -p dllm-llama-server`, or set DLLMD_RUNTIME_BIN to use an external runtime instead",
            candidate.display()
        )
        .into());
    }
    Ok(candidate)
}

fn peer_config(
    store: &NetworkStore,
    owner_key_path: &Path,
) -> Result<Option<PeerNodeConfig>, Box<dyn std::error::Error>> {
    if !env_bool("DLLMD_P2P_ENABLED", false)? {
        return Ok(None);
    }
    let key_path = resolve_transport_key_path()?;
    let local_node_key_path = std::env::var("DLLMD_NODE_KEY")
        .map(PathBuf::from)
        .unwrap_or_else(|_| owner_key_path.to_path_buf());
    let local_node = NetworkStore::load_owner_key(local_node_key_path)?
        .verifying_key()
        .to_bytes();
    let transport_key = load_or_create_identity(&key_path)?;
    let local_peer = transport_key.public().to_peer_id();
    match store.authorize_transport_endpoint(local_node, &local_peer.to_string(), now_unix()) {
        Ok(_) => {}
        Err(StoreError::TransportIdentityUnauthorized) => {
            println!(
                "P2P enabled but this node is not yet authorized (waiting on owner approval) -- continuing without P2P"
            );
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let bootstrap = std::env::var("DLLMD_P2P_BOOTSTRAP")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .map(str::parse::<Multiaddr>)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let mut eligible_forwarders = HashSet::new();
    for policy in &store.state.state.forwarding_policy {
        if let Some(binding) = store
            .state
            .state
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == policy.node_pubkey)
        {
            if binding.expires_at_unix > now_unix() {
                eligible_forwarders.insert(binding.transport_peer_id.parse::<PeerId>()?);
            }
        }
    }
    let local_policy = store
        .state
        .state
        .forwarding_policy
        .iter()
        .find(|policy| policy.node_pubkey == local_node);
    let forwarding_enabled = local_policy.is_some();
    let reserve_default =
        !forwarding_enabled && bootstrap.as_ref().is_some_and(|list| !list.is_empty());
    let discovery_mode = match std::env::var("DLLMD_P2P_DISCOVERY_MODE").ok().as_deref() {
        None | Some("listed") => DiscoveryMode::Listed,
        Some("unlisted") => DiscoveryMode::Unlisted,
        Some(other) => {
            return Err(format!(
                "DLLMD_P2P_DISCOVERY_MODE must be \"listed\" or \"unlisted\", got \"{other}\""
            )
            .into());
        }
    };
    Ok(Some(PeerNodeConfig {
        key_path,
        listen_port: std::env::var("DLLMD_P2P_PORT")
            .ok()
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or(7444),
        bootstrap: bootstrap.unwrap_or_default(),
        forwarding_enabled,
        max_reservations: local_policy
            .map(|policy| policy.max_reservations as usize)
            .unwrap_or(0),
        eligible_forwarders,
        reserve_forwarding_path: env_bool("DLLMD_P2P_RESERVE", reserve_default)?,
        discovery_mode,
        dht_hosting: env_bool("DLLMD_P2P_DHT_HOSTING", true)?,
        max_established_incoming: std::env::var("DLLMD_P2P_MAX_ESTABLISHED_INCOMING")
            .ok()
            .and_then(|v| v.parse().ok()),
        max_established_per_peer: std::env::var("DLLMD_P2P_MAX_ESTABLISHED_PER_PEER")
            .ok()
            .and_then(|v| v.parse().ok()),
        max_pending_incoming: std::env::var("DLLMD_P2P_MAX_PENDING_INCOMING")
            .ok()
            .and_then(|v| v.parse().ok()),
        reservation_rate_limit: None,
        circuit_src_rate_limit: None,
    }))
}

fn resolve_transport_key_path() -> std::io::Result<PathBuf> {
    match std::env::var("DLLMD_P2P_KEY") {
        Ok(value) => Ok(PathBuf::from(value)),
        Err(_) => dllm_daemon::default_transport_key_path(),
    }
}

/// Builds a `PeerNodeConfig` (via `peer_config`) and starts the peer
/// subsystem if the local node is currently authorized. Returns `Ok(None)`
/// if P2P isn't enabled or the node isn't authorized yet -- same meaning as
/// `peer_config` returning `None`. Reusable from both initial startup and
/// the watcher in `spawn_peer_watcher`, since both need to do exactly this.
fn try_start_peer(
    store: &NetworkStore,
    owner_key_path: &Path,
    admission_limit: usize,
) -> Result<
    Option<(PeerNodeHandle, dllm_daemon::peer_service::PeerBundle)>,
    Box<dyn std::error::Error>,
> {
    let Some(config) = peer_config(store, owner_key_path)? else {
        return Ok(None);
    };
    let handle = start_peer_node(config)?;
    let diagnostics = handle.diagnostics();
    let state_snapshot = Arc::new(store.state.state.clone());
    let (_auth_tx, auth_rx) = tokio::sync::watch::channel(state_snapshot);
    let auth_view = dllm_transport::auth::AuthView::new(auth_rx);
    let admission = Arc::new(Semaphore::new(admission_limit));
    let client =
        dllm_daemon::peer_service::PeerClient::new(handle.clone(), auth_view.clone(), admission);
    Ok(Some((
        handle,
        dllm_daemon::peer_service::PeerBundle {
            diagnostics,
            client,
            auth_view,
        },
    )))
}

/// Watches `state_path`'s parent directory for changes (the file itself is
/// replaced via a temp-file-then-rename, so watching the directory catches
/// that reliably). On every change touching `state_path`, reloads it and
/// retries `try_start_peer`; once authorized, swaps the result into
/// `peer_bundle` (visible to every in-flight `ApiState` clone through the
/// shared `RwLock`), spawns its dispatcher, and stops watching.
fn spawn_peer_watcher(
    state_path: PathBuf,
    owner_key_path: PathBuf,
    admission_limit: usize,
    peer_bundle: Arc<tokio::sync::RwLock<Option<dllm_daemon::peer_service::PeerBundle>>>,
    api_state: api::ApiState,
) {
    tokio::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<notify::Result<notify::Event>>(16);
        let watch_dir = state_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let mut watcher = match notify::recommended_watcher(move |event| {
            let _ = tx.blocking_send(event);
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                println!("peer watcher: failed to start file watcher: {error}");
                return;
            }
        };
        if let Err(error) = notify::Watcher::watch(
            &mut watcher,
            &watch_dir,
            notify::RecursiveMode::NonRecursive,
        ) {
            println!(
                "peer watcher: failed to watch {}: {error}",
                watch_dir.display()
            );
            return;
        }
        let state_file_name = state_path.file_name().map(|name| name.to_owned());
        while let Some(event) = rx.recv().await {
            let Ok(event) = event else { continue };
            let touches_state_file = event
                .paths
                .iter()
                .any(|path| path.file_name() == state_file_name.as_deref());
            if !touches_state_file {
                continue;
            }
            let reloaded = if owner_key_path.exists() {
                NetworkStore::load(&state_path, &owner_key_path)
            } else {
                NetworkStore::load_replica(&state_path)
            };
            let Ok(reloaded) = reloaded else { continue };
            let outcome = try_start_peer(&reloaded, &owner_key_path, admission_limit)
                .map_err(|error| error.to_string());
            match outcome {
                Ok(Some((handle, bundle))) => {
                    let client = bundle.client.clone();
                    *peer_bundle.write().await = Some(bundle);
                    drop(handle);
                    let _dispatcher =
                        dllm_daemon::peer_service::spawn_dispatcher(client, api_state.clone());
                    println!("P2P authorized -- peer transport is now active");
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    println!("peer watcher: retry failed: {error}");
                }
            }
        }
    });
}

fn env_bool(name: &str, default: bool) -> Result<bool, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => match value.as_str() {
            "1" | "true" | "yes" => Ok(true),
            "0" | "false" | "no" => Ok(false),
            _ => Err(format!("{name} must be true or false").into()),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn has_management_access(
    legacy_token: &Option<String>,
    credentials: &[ManagementCredential],
) -> bool {
    legacy_token.as_ref().is_some_and(|token| !token.is_empty())
        || credentials
            .iter()
            .any(|credential| !credential.token.is_empty())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
