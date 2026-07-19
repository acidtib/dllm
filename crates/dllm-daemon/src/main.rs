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
use dllm_protocol::{now_unix, CpuCapability, HardwareBenchmark, HardwareProfile};
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
    if std::env::args().any(|argument| argument == "--help" || argument == "-h") {
        println!("dllmd\n\nSelf-hosted inference network daemon\n\nConfiguration is provided through DLLMD_* environment variables. See docs/getting-started.md.");
        return Ok(());
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bind = std::env::var("DLLMD_BIND").unwrap_or_else(|_| "127.0.0.1:7337".into());
    let state_path = match std::env::var("DLLMD_STATE") {
        Ok(value) => PathBuf::from(value),
        Err(_) => dllm_daemon::default_state_path()?,
    };
    let owner_key_path = if let Ok(value) = std::env::var("DLLMD_AUTHORITY_KEY") {
        PathBuf::from(value)
    } else if let Ok(value) = std::env::var("DLLMD_OWNER_KEY") {
        eprintln!("warning: DLLMD_OWNER_KEY is deprecated, use DLLMD_AUTHORITY_KEY");
        PathBuf::from(value)
    } else {
        let authority = dllm_daemon::default_authority_key_path()?;
        let legacy = dllm_daemon::default_owner_key_path()?;
        if dllm_daemon::migrate_legacy_authority_key(&authority, &legacy)? {
            eprintln!("warning: migrated legacy owner.key to authority.key");
        }
        authority
    };
    let network_name =
        std::env::var("DLLMD_NETWORK").unwrap_or_else(|_| dllm_daemon::generate_network_name());
    let joining_at_start = std::env::var("DLLMD_JOIN_URL").is_ok();
    let node_key_path = std::env::var("DLLMD_NODE_KEY")
        .map(PathBuf::from)
        .unwrap_or(dllm_daemon::default_node_key_path()?);
    let node_key = dllm_daemon::load_or_create_node_key(&node_key_path)?;
    let transport_key_path = resolve_transport_key_path()?;
    load_or_create_identity(&transport_key_path)?;
    let config_path = dllm_daemon::default_config_path()?;
    let mut runtime_url = std::env::var("DLLMD_RUNTIME_URL").ok();
    let admission_limit = parse_env("DLLMD_ADMISSION_LIMIT", 1);
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
    let bootstrap_multiaddrs = advertised_p2p_addresses()?;
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
    let state_preexisted = state_path.exists();
    let store = if state_preexisted {
        if owner_key_path.exists() {
            NetworkStore::load(&state_path, &owner_key_path)?
        } else {
            NetworkStore::load_replica(&state_path)?
        }
    } else {
        let mut store = NetworkStore::create(network_name);
        store.save_owner_key(&owner_key_path)?;
        if env_bool("DLLMD_P2P_ENABLED", true)? && !joining_at_start {
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
                issued_at + dllm_daemon::AUTOMATIC_BINDING_LIFETIME_SECS,
            )?;
        }
        store.save(&state_path)?;
        store
    };
    let provisional_marker_path = state_path.with_extension("provisional.json");
    if !state_preexisted {
        write_provisional_marker(&provisional_marker_path, &store)?;
    }
    let p2p_requested = env_bool("DLLMD_P2P_ENABLED", true)? && !joining_at_start;
    let peer_bundle: Arc<tokio::sync::RwLock<Option<dllm_daemon::peer_service::PeerBundle>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    let peer_handle: Arc<Mutex<Option<PeerNodeHandle>>> = Arc::new(Mutex::new(None));
    if p2p_requested {
        if let Some((handle, bundle)) = try_start_peer(&store, &owner_key_path, admission_limit)? {
            *peer_handle.lock().await = Some(handle);
            *peer_bundle.write().await = Some(bundle);
        }
    }

    let mut runtime_worker = None;
    let mut pending_benchmark = None;
    if !joining_at_start {
        let node_pubkey = node_key.verifying_key().to_bytes();
        let existing_benchmarks: Vec<HardwareBenchmark> = store
            .state
            .state
            .hardware_profiles
            .iter()
            .find(|profile| profile.node_pubkey == node_pubkey)
            .map(|profile| profile.benchmarks.clone())
            .unwrap_or_default();
        let activation = start_configured_runtime(&existing_benchmarks).await?;
        runtime_url = activation.runtime_url;
        runtime_worker = activation.runtime_worker;
        pending_benchmark = activation.benchmark_context;
    }
    let runtime_url = Arc::new(tokio::sync::RwLock::new(runtime_url));
    let runtime_worker = Arc::new(Mutex::new(runtime_worker));
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
        bootstrap_multiaddrs: bootstrap_multiaddrs.clone(),
        node_key_path: node_key_path.clone(),
        transport_key_path: transport_key_path.clone(),
        config_path: config_path.clone(),
        authority_key_path: owner_key_path.clone(),
        provisional_marker_path: provisional_marker_path.clone(),
        onboarding: Arc::new(tokio::sync::RwLock::new(api::OnboardingStatus::Inactive)),
        replica_loads: Arc::new(Mutex::new(HashMap::new())),
        peer_nonces: Arc::new(Mutex::new(HashMap::new())),
        peer_quota: Arc::new(Semaphore::new(admission_limit)),
        peer: peer_bundle.clone(),
        peer_handle: peer_handle.clone(),
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
    if let Some((fit, model_label, context_size)) = pending_benchmark {
        if let Some(runtime_url) = primary_state.runtime_url.read().await.clone() {
            let node_pubkey = node_key.verifying_key().to_bytes();
            let benchmark_state = primary_state.clone();
            tokio::spawn(benchmark_and_publish(
                benchmark_state,
                node_pubkey,
                runtime_url,
                model_label,
                fit,
                context_size,
            ));
        }
    }
    if let Ok(authority_url) = std::env::var("DLLMD_JOIN_URL") {
        let _ = api::start_onboarding_workflow(primary_state.clone(), authority_url)
            .await
            .map_err(|(_, message)| message)?;
        spawn_runtime_activation(
            primary_state.onboarding.clone(),
            runtime_url.clone(),
            runtime_worker.clone(),
        );
    }
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
                bootstrap_multiaddrs: Vec::new(),
                node_key_path: node_key_path.clone(),
                transport_key_path: transport_key_path.clone(),
                config_path: config_path.clone(),
                authority_key_path: config.owner_key_path.clone(),
                provisional_marker_path: additional_state_path.with_extension("provisional.json"),
                onboarding: Arc::new(tokio::sync::RwLock::new(api::OnboardingStatus::Inactive)),
                replica_loads: Arc::new(Mutex::new(HashMap::new())),
                peer_nonces: Arc::new(Mutex::new(HashMap::new())),
                peer_quota: Arc::new(Semaphore::new(admission_limit)),
                peer: Arc::new(tokio::sync::RwLock::new(None)),
                peer_handle: Arc::new(Mutex::new(None)),
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
    if p2p_requested || joining_at_start {
        spawn_peer_watcher(
            state_path.clone(),
            owner_key_path.clone(),
            admission_limit,
            peer_bundle.clone(),
            primary_state.clone(),
            peer_handle.clone(),
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
    if let Some(worker) = runtime_worker.lock().await.take() {
        worker.shutdown().await?;
    }
    if let Some(peer) = peer_handle.lock().await.take() {
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

fn resolve_gpu_config(
    explicit_gpu_layers: Option<u32>,
    explicit_context_size: Option<u32>,
    cached: Option<&HardwareBenchmark>,
    fit: Option<&dllm_runtime::FitReport>,
) -> (u32, u32) {
    let gpu_layers = explicit_gpu_layers
        .or_else(|| cached.map(|benchmark| benchmark.gpu_layers))
        .or_else(|| fit.map(|report| report.n_gpu_layers))
        .unwrap_or(38);
    let context_size = explicit_context_size
        .or_else(|| cached.map(|benchmark| benchmark.context_size))
        .or_else(|| fit.map(|report| report.n_ctx))
        .unwrap_or(2048);
    (gpu_layers, context_size)
}

fn merge_benchmark_into_profile(
    existing: Option<HardwareProfile>,
    node_pubkey: [u8; 32],
    benchmark: HardwareBenchmark,
) -> HardwareProfile {
    let mut profile = existing.unwrap_or_else(|| HardwareProfile {
        node_pubkey,
        observed_at_unix: 0,
        cpu: CpuCapability {
            model: String::new(),
            physical_cores: 0,
            logical_cores: 0,
            features: vec![],
        },
        system_memory_bytes: 0,
        available_memory_bytes: 0,
        accelerators: vec![],
        runtimes: vec![],
        benchmarks: vec![],
    });
    profile.observed_at_unix = now_unix();
    profile.benchmarks.retain(|candidate| {
        !(candidate.model == benchmark.model && candidate.backend == benchmark.backend)
    });
    profile.benchmarks.push(benchmark);
    profile
}

struct MeasuredThroughput {
    prompt_tokens_per_second_milli: u64,
    decode_tokens_per_second_milli: u64,
}

const BENCHMARK_PROMPT: &str = "The quick brown fox jumps over the lazy dog. Describe what \
    happens next in exactly one sentence, staying factual and concise.";

async fn measure_benchmark(
    client: &reqwest::Client,
    runtime_url: &str,
) -> Result<MeasuredThroughput, Box<dyn std::error::Error>> {
    let tokenize: serde_json::Value = client
        .post(format!("{runtime_url}/tokenize"))
        .json(&serde_json::json!({ "content": BENCHMARK_PROMPT }))
        .send()
        .await?
        .json()
        .await?;
    let prompt_tokens = tokenize["tokens"].as_array().map_or(0, Vec::len) as u64;

    let prefill_start = std::time::Instant::now();
    client
        .post(format!("{runtime_url}/v1/chat/completions"))
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": BENCHMARK_PROMPT}],
            "max_tokens": 1,
            "stream": false,
        }))
        .send()
        .await?
        .error_for_status()?;
    let prefill_elapsed = prefill_start.elapsed().as_secs_f64();

    let decode_start = std::time::Instant::now();
    let decode_response: serde_json::Value = client
        .post(format!("{runtime_url}/v1/chat/completions"))
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": "Count from one to twenty."}],
            "max_tokens": 64,
            "stream": false,
        }))
        .send()
        .await?
        .json()
        .await?;
    let decode_elapsed = decode_start.elapsed().as_secs_f64();
    let completion_tokens = decode_response["usage"]["completion_tokens"]
        .as_u64()
        .unwrap_or(0);

    Ok(MeasuredThroughput {
        prompt_tokens_per_second_milli: if prefill_elapsed > 0.0 {
            (prompt_tokens as f64 / prefill_elapsed * 1000.0) as u64
        } else {
            0
        },
        decode_tokens_per_second_milli: if decode_elapsed > 0.0 {
            (completion_tokens as f64 / decode_elapsed * 1000.0) as u64
        } else {
            0
        },
    })
}

async fn benchmark_and_publish(
    state: api::ApiState,
    node_pubkey: [u8; 32],
    runtime_url: String,
    model_label: String,
    fit: dllm_runtime::FitReport,
    context_size: u32,
) {
    let already_benchmarked = state
        .store
        .lock()
        .await
        .state
        .state
        .hardware_profiles
        .iter()
        .find(|profile| profile.node_pubkey == node_pubkey)
        .is_some_and(|profile| {
            profile
                .benchmarks
                .iter()
                .any(|benchmark| benchmark.model == model_label && benchmark.backend == fit.backend)
        });
    if already_benchmarked {
        return;
    }
    let measured = match measure_benchmark(&state.client, &runtime_url).await {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("hardware benchmark failed: {error}");
            return;
        }
    };
    let benchmark = HardwareBenchmark {
        model: model_label,
        backend: fit.backend,
        gpu_layers: fit.n_gpu_layers,
        context_size,
        concurrency: 1,
        prompt_tokens_per_second_milli: measured.prompt_tokens_per_second_milli,
        decode_tokens_per_second_milli: measured.decode_tokens_per_second_milli,
        // Pre-flight projection from fit_params/get_device_memory_data, not a
        // measurement of the running worker: measure_benchmark only measures
        // throughput, and the worker exposes no live memory query.
        peak_memory_bytes: fit.peak_memory_bytes,
    };
    let mut store = state.store.lock().await;
    let existing = store
        .state
        .state
        .hardware_profiles
        .iter()
        .find(|profile| profile.node_pubkey == node_pubkey)
        .cloned();
    let profile = merge_benchmark_into_profile(existing, node_pubkey, benchmark);
    match store.publish_hardware_profile(profile) {
        Ok(true) => {
            if let Err(error) = store.save(&state.state_path) {
                eprintln!("failed to save hardware profile: {error}");
            }
        }
        Ok(false) => {}
        Err(StoreError::OwnerAuthorityUnavailable) => {
            println!(
                "hardware benchmark complete; publishing to network state requires the \
                 network owner, skipped on this member node"
            );
        }
        Err(error) => eprintln!("failed to publish hardware profile: {error}"),
    }
}

struct RuntimeActivation {
    runtime_url: Option<String>,
    runtime_worker: Option<RuntimeWorker>,
    benchmark_context: Option<(dllm_runtime::FitReport, String, u32)>,
}

async fn start_configured_runtime(
    existing_benchmarks: &[HardwareBenchmark],
) -> Result<RuntimeActivation, Box<dyn std::error::Error>> {
    if let Ok(runtime_url) = std::env::var("DLLMD_RUNTIME_URL") {
        return Ok(RuntimeActivation {
            runtime_url: Some(runtime_url),
            runtime_worker: None,
            benchmark_context: None,
        });
    }
    if let Ok(binary) = std::env::var("DLLMD_RUNTIME_BIN") {
        let Ok(model) = std::env::var("DLLMD_MODEL_PATH") else {
            return Ok(RuntimeActivation {
                runtime_url: None,
                runtime_worker: None,
                benchmark_context: None,
            });
        };
        let config = LlamaCppConfig {
            binary: binary.into(),
            model: model.into(),
            host: "127.0.0.1".into(),
            port: parse_env("DLLMD_RUNTIME_PORT", 8081),
            gpu_layers: std::env::var("DLLMD_GPU_LAYERS").unwrap_or_else(|_| "38".into()),
            context_size: parse_env("DLLMD_CONTEXT_SIZE", 2048),
            extra_args: vec![],
        };
        let worker = RuntimeWorker::start(&config, Duration::from_secs(300)).await?;
        return Ok(RuntimeActivation {
            runtime_url: Some(worker.endpoint().to_owned()),
            runtime_worker: Some(worker),
            benchmark_context: None,
        });
    }

    let model_path = std::env::var("DLLMD_MODEL_PATH").ok();
    let hf_model = std::env::var("DLLMD_HF_MODEL").ok();
    let model = match (model_path, hf_model) {
        (Some(_), Some(_)) => {
            return Err("DLLMD_MODEL_PATH and DLLMD_HF_MODEL are mutually exclusive".into());
        }
        (Some(path), None) => Some(BundledModelSource::Local(path.into())),
        (None, Some(repo)) => Some(BundledModelSource::HuggingFace(repo)),
        (None, None) => None,
    };
    let Some(model) = model else {
        return Ok(RuntimeActivation {
            runtime_url: None,
            runtime_worker: None,
            benchmark_context: None,
        });
    };
    let model_label = match &model {
        BundledModelSource::Local(path) => path.display().to_string(),
        BundledModelSource::HuggingFace(repo) => repo.clone(),
    };
    let hf_home = if matches!(model, BundledModelSource::HuggingFace(_))
        && std::env::var("HF_HOME").is_err()
    {
        Some(dllm_daemon::default_dir()?.join("models"))
    } else {
        None
    };
    let explicit_gpu_layers: Option<u32> = std::env::var("DLLMD_GPU_LAYERS")
        .ok()
        .and_then(|value| value.parse().ok());
    let explicit_context_size: Option<u32> = std::env::var("DLLMD_CONTEXT_SIZE")
        .ok()
        .and_then(|value| value.parse().ok());
    let cached_benchmark = existing_benchmarks
        .iter()
        .find(|benchmark| benchmark.model == model_label)
        .cloned();
    // A cached benchmark already answers gpu_layers/context_size and was already
    // published, so skip both the fit subprocess and re-benchmarking. Otherwise
    // always fit (even when both env vars are explicit) so a fresh benchmark still
    // runs after startup: throughput measurement needs fit's `backend` label, which
    // dllmd has no other way to learn (see crates/dllm-runtime's FitReport).
    let fit_report = if cached_benchmark.is_none() {
        let request = dllm_runtime::FitRequest {
            binary: bundled_runtime_binary()?,
            model: model.clone(),
            n_ctx_min: parse_env("DLLMD_FIT_N_CTX_MIN", 4096),
            margin_bytes: parse_env("DLLMD_FIT_MARGIN_BYTES", 1_073_741_824),
        };
        match dllm_runtime::run_fit(&request).await {
            Ok(report) => Some(report),
            Err(error) => {
                eprintln!("hardware auto-fit failed, falling back to defaults: {error}");
                None
            }
        }
    } else {
        None
    };
    let (gpu_layers, context_size) = resolve_gpu_config(
        explicit_gpu_layers,
        explicit_context_size,
        cached_benchmark.as_ref(),
        fit_report.as_ref(),
    );
    let config = BundledRuntimeConfig {
        binary: bundled_runtime_binary()?,
        model,
        host: "127.0.0.1".into(),
        port: parse_env("DLLMD_RUNTIME_PORT", 8081),
        gpu_layers,
        context_size: Some(context_size),
        api_key: None,
        parallel: 1,
        mmproj: std::env::var("DLLMD_MMPROJ_PATH").ok().map(PathBuf::from),
        hf_home,
    };
    let benchmark_context = fit_report.map(|report| (report, model_label, context_size));
    let worker = RuntimeWorker::start_bundled(&config, Duration::from_secs(300)).await?;
    Ok(RuntimeActivation {
        runtime_url: Some(worker.endpoint().to_owned()),
        runtime_worker: Some(worker),
        benchmark_context,
    })
}

fn spawn_runtime_activation(
    onboarding: Arc<tokio::sync::RwLock<api::OnboardingStatus>>,
    runtime_url: Arc<tokio::sync::RwLock<Option<String>>>,
    runtime_worker: Arc<Mutex<Option<RuntimeWorker>>>,
) {
    tokio::spawn(async move {
        loop {
            let status = onboarding.read().await.clone();
            match status {
                api::OnboardingStatus::Active { .. } => break,
                api::OnboardingStatus::Failed { detail, .. } => {
                    eprintln!("runtime activation skipped: onboarding failed: {detail}");
                    return;
                }
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
        let activation = start_configured_runtime(&[])
            .await
            .map_err(|error| error.to_string());
        match activation {
            Ok(activation) => {
                *runtime_url.write().await = activation.runtime_url;
                *runtime_worker.lock().await = activation.runtime_worker;
                println!("onboarding activation completed");
            }
            Err(error) => {
                eprintln!("runtime activation failed: {error}");
            }
        }
    });
}

fn advertised_p2p_addresses() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let Some(value) = std::env::var("DLLMD_P2P_ADVERTISE")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(Vec::new());
    };
    let transport_key = load_or_create_identity(&resolve_transport_key_path()?)?;
    let peer_id = transport_key.public().to_peer_id();
    value
        .split(',')
        .map(str::trim)
        .map(|address| {
            let address: Multiaddr = address.parse()?;
            let rendered = address.to_string();
            if rendered.contains("/ip4/0.0.0.0/") || rendered.contains("/ip6/::/") {
                return Err("DLLMD_P2P_ADVERTISE must contain dialable addresses".into());
            }
            let with_peer = if rendered.contains("/p2p/") {
                if !rendered.ends_with(&format!("/p2p/{peer_id}")) {
                    return Err("advertised P2P address contains a different peer identity".into());
                }
                rendered
            } else {
                format!("{rendered}/p2p/{peer_id}")
            };
            with_peer.parse::<Multiaddr>()?;
            Ok(with_peer)
        })
        .collect()
}

fn write_provisional_marker(
    path: &Path,
    store: &NetworkStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(
        temporary.path(),
        serde_json::to_vec_pretty(&serde_json::json!({
            "generation": store.state.state.generation,
            "authority_pubkey": store.state.state.owner_pubkey,
        }))?,
    )?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn peer_config(
    store: &NetworkStore,
    owner_key_path: &Path,
) -> Result<Option<PeerNodeConfig>, Box<dyn std::error::Error>> {
    if !env_bool("DLLMD_P2P_ENABLED", true)? {
        return Ok(None);
    }
    let key_path = resolve_transport_key_path()?;
    let local_node_key_path = if let Ok(path) = std::env::var("DLLMD_NODE_KEY") {
        PathBuf::from(path)
    } else if owner_key_path.exists()
        && NetworkStore::load_owner_key(owner_key_path)?
            .verifying_key()
            .to_bytes()
            == store.state.state.owner_pubkey
    {
        owner_key_path.to_path_buf()
    } else {
        dllm_daemon::default_node_key_path()?
    };
    let local_node = NetworkStore::load_owner_key(local_node_key_path)?
        .verifying_key()
        .to_bytes();
    let transport_key = load_or_create_identity(&key_path)?;
    let local_peer = transport_key.public().to_peer_id();
    match store.authorize_transport_endpoint(local_node, &local_peer.to_string(), now_unix()) {
        Ok(_) => {}
        Err(StoreError::TransportIdentityUnauthorized) => {
            println!(
                "P2P enabled but this node is not yet authorized, waiting on authority approval"
            );
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    }

    let environment_bootstrap = std::env::var("DLLMD_P2P_BOOTSTRAP")
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
    let config_bootstrap = dllm_daemon::default_config_path()
        .ok()
        .and_then(|path| dllm_daemon::local_config::LocalConfig::load(&path).ok())
        .and_then(|config| config.p2p_bootstrap)
        .filter(|addresses| !addresses.is_empty())
        .map(|addresses| {
            addresses
                .into_iter()
                .map(|address| address.parse::<Multiaddr>())
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let bootstrap = environment_bootstrap.or(config_bootstrap);
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
    peer_handle: Arc<Mutex<Option<PeerNodeHandle>>>,
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
        if try_activate_watched_peer(
            &state_path,
            &owner_key_path,
            admission_limit,
            &peer_bundle,
            &api_state,
            &peer_handle,
        )
        .await
        {
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
            if try_activate_watched_peer(
                &state_path,
                &owner_key_path,
                admission_limit,
                &peer_bundle,
                &api_state,
                &peer_handle,
            )
            .await
            {
                break;
            }
        }
    });
}

async fn try_activate_watched_peer(
    state_path: &Path,
    owner_key_path: &Path,
    admission_limit: usize,
    peer_bundle: &Arc<tokio::sync::RwLock<Option<dllm_daemon::peer_service::PeerBundle>>>,
    api_state: &api::ApiState,
    peer_handle: &Arc<Mutex<Option<PeerNodeHandle>>>,
) -> bool {
    if peer_bundle.read().await.is_some() {
        return false;
    }
    let reloaded = NetworkStore::load(state_path, owner_key_path)
        .or_else(|_| NetworkStore::load_replica(state_path));
    let Ok(reloaded) = reloaded else {
        return false;
    };
    match try_start_peer(&reloaded, owner_key_path, admission_limit)
        .map_err(|error| error.to_string())
    {
        Ok(Some((handle, bundle))) => {
            let client = bundle.client.clone();
            *peer_bundle.write().await = Some(bundle);
            *peer_handle.lock().await = Some(handle);
            let _dispatcher =
                dllm_daemon::peer_service::spawn_dispatcher(client, api_state.clone());
            println!("P2P authorized -- peer transport is now active");
            true
        }
        Ok(None) => false,
        Err(error) => {
            println!("peer watcher: retry failed: {error}");
            false
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_activation_waits_until_onboarding_is_active() {
        let onboarding = Arc::new(tokio::sync::RwLock::new(api::OnboardingStatus::Joining {
            authority_url: "https://authority.example".into(),
            detail: "waiting".into(),
        }));
        let runtime_url = Arc::new(tokio::sync::RwLock::new(Some("not-started".into())));
        let runtime_worker = Arc::new(Mutex::new(None));
        spawn_runtime_activation(onboarding.clone(), runtime_url.clone(), runtime_worker);

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(runtime_url.read().await.as_deref(), Some("not-started"));

        *onboarding.write().await = api::OnboardingStatus::Active {
            authority_url: "https://authority.example".into(),
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if runtime_url.read().await.as_deref() != Some("not-started") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(runtime_url.read().await.is_none());
    }

    #[test]
    fn merge_benchmark_into_profile_replaces_matching_entry_and_keeps_others() {
        let node_pubkey = [7u8; 32];
        let existing = dllm_protocol::HardwareProfile {
            node_pubkey,
            observed_at_unix: 1,
            cpu: dllm_protocol::CpuCapability {
                model: "operator-reported cpu".into(),
                physical_cores: 8,
                logical_cores: 16,
                features: vec![],
            },
            system_memory_bytes: 32_000_000_000,
            available_memory_bytes: 20_000_000_000,
            accelerators: vec![],
            runtimes: vec![],
            benchmarks: vec![dllm_protocol::HardwareBenchmark {
                model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
                backend: "vulkan".into(),
                gpu_layers: 18,
                context_size: 2048,
                concurrency: 1,
                prompt_tokens_per_second_milli: 1_000,
                decode_tokens_per_second_milli: 500,
                peak_memory_bytes: 1,
            }],
        };
        let new_benchmark = dllm_protocol::HardwareBenchmark {
            model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
            backend: "cuda".into(),
            gpu_layers: 32,
            context_size: 8192,
            concurrency: 1,
            prompt_tokens_per_second_milli: 9_000,
            decode_tokens_per_second_milli: 4_000,
            peak_memory_bytes: 4_200_000_000,
        };
        let merged =
            merge_benchmark_into_profile(Some(existing), node_pubkey, new_benchmark.clone());
        assert_eq!(merged.cpu.model, "operator-reported cpu");
        assert_eq!(merged.benchmarks.len(), 2);
        assert!(merged.benchmarks.contains(&new_benchmark));

        let replacement = dllm_protocol::HardwareBenchmark {
            backend: "vulkan".into(),
            decode_tokens_per_second_milli: 600,
            ..new_benchmark.clone()
        };
        let merged_again =
            merge_benchmark_into_profile(Some(merged), node_pubkey, replacement.clone());
        assert_eq!(merged_again.benchmarks.len(), 2);
        assert!(merged_again.benchmarks.contains(&replacement));
        assert!(!merged_again
            .benchmarks
            .iter()
            .any(|b| b.backend == "vulkan" && b.decode_tokens_per_second_milli == 500));
    }

    #[test]
    fn merge_benchmark_into_profile_creates_fresh_profile_when_none_exists() {
        let node_pubkey = [9u8; 32];
        let benchmark = dllm_protocol::HardwareBenchmark {
            model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
            backend: "cuda".into(),
            gpu_layers: 32,
            context_size: 8192,
            concurrency: 1,
            prompt_tokens_per_second_milli: 9_000,
            decode_tokens_per_second_milli: 4_000,
            peak_memory_bytes: 4_200_000_000,
        };

        let profile = merge_benchmark_into_profile(None, node_pubkey, benchmark.clone());

        assert_eq!(profile.node_pubkey, node_pubkey);
        assert_eq!(profile.benchmarks, vec![benchmark]);
        assert_eq!(profile.cpu.model, "");
        assert_eq!(profile.system_memory_bytes, 0);
        assert!(profile.observed_at_unix > 0);
    }

    #[tokio::test]
    async fn measure_benchmark_computes_positive_throughput_from_a_stub_server() {
        let app = axum::Router::new()
            .route(
                "/tokenize",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({ "tokens": [1, 2, 3, 4, 5] }))
                }),
            )
            .route(
                "/v1/chat/completions",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}],
                        "usage": {"prompt_tokens": 0, "completion_tokens": 8, "total_tokens": 8}
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let result = measure_benchmark(&client, &format!("http://{addr}"))
            .await
            .unwrap();
        assert!(result.prompt_tokens_per_second_milli > 0);
        assert!(result.decode_tokens_per_second_milli > 0);
    }

    #[test]
    fn resolve_gpu_config_prefers_explicit_over_cached_over_fit_over_fallback() {
        let fit = dllm_runtime::FitReport {
            n_gpu_layers: 20,
            n_ctx: 8192,
            peak_memory_bytes: 1,
            backend: "cuda".into(),
        };
        let cached = dllm_protocol::HardwareBenchmark {
            model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
            backend: "cuda".into(),
            gpu_layers: 30,
            context_size: 4096,
            concurrency: 1,
            prompt_tokens_per_second_milli: 1,
            decode_tokens_per_second_milli: 1,
            peak_memory_bytes: 1,
        };

        // both explicit: ignores cached and fit entirely
        assert_eq!(
            resolve_gpu_config(Some(99), Some(1234), Some(&cached), Some(&fit)),
            (99, 1234)
        );
        // one explicit, one from cache
        assert_eq!(
            resolve_gpu_config(Some(99), None, Some(&cached), Some(&fit)),
            (99, 4096)
        );
        assert_eq!(
            resolve_gpu_config(None, Some(1024), Some(&cached), Some(&fit)),
            (30, 1024)
        );
        // neither explicit, cached wins over fit
        assert_eq!(
            resolve_gpu_config(None, None, Some(&cached), Some(&fit)),
            (30, 4096)
        );
        // no cache, falls through to fit
        assert_eq!(resolve_gpu_config(None, None, None, Some(&fit)), (20, 8192));
        // nothing available, hardcoded fallback
        assert_eq!(resolve_gpu_config(None, None, None, None), (38, 2048));
    }

    #[test]
    fn parse_env_falls_back_to_default_on_missing_or_invalid_value() {
        assert_eq!(parse_env::<u32>("DLLM_TEST_PARSE_ENV_UNSET_VAR", 7), 7);
        std::env::set_var("DLLM_TEST_PARSE_ENV_VALID", "42");
        assert_eq!(parse_env::<u32>("DLLM_TEST_PARSE_ENV_VALID", 7), 42);
        std::env::set_var("DLLM_TEST_PARSE_ENV_INVALID", "not-a-number");
        assert_eq!(parse_env::<u32>("DLLM_TEST_PARSE_ENV_INVALID", 7), 7);
        std::env::remove_var("DLLM_TEST_PARSE_ENV_VALID");
        std::env::remove_var("DLLM_TEST_PARSE_ENV_INVALID");
    }
}
