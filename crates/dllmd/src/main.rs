use axum_server::{tls_rustls::RustlsConfig, Handle};
use dllm_runtime::{LlamaCppConfig, RuntimeWorker};
use dllmd::{
    api,
    credentials::{CredentialRegistry, ManagementCredential},
    inference::{InferenceCredential, InferenceRegistry},
    NetworkStore,
};
use serde::Deserialize;
use std::time::Duration;
use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};
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
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bind = std::env::var("DLLMD_BIND").unwrap_or_else(|_| "127.0.0.1:7337".into());
    let state_path =
        PathBuf::from(std::env::var("DLLMD_STATE").unwrap_or_else(|_| "dllm-state.json".into()));
    let owner_key_path =
        PathBuf::from(std::env::var("DLLMD_OWNER_KEY").unwrap_or_else(|_| "dllm-owner.key".into()));
    let network_name = std::env::var("DLLMD_NETWORK").unwrap_or_else(|_| "private".into());
    let mut runtime_url = std::env::var("DLLMD_RUNTIME_URL").ok();
    let admission_limit = std::env::var("DLLMD_ADMISSION_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let management_token = std::env::var("DLLMD_MANAGEMENT_TOKEN").ok();
    let management_credentials = std::env::var("DLLMD_MANAGEMENT_CREDENTIALS")
        .ok()
        .map(|value| serde_json::from_str::<Vec<ManagementCredential>>(&value))
        .transpose()?
        .unwrap_or_default();
    let management_credentials_path = std::env::var("DLLMD_MANAGEMENT_CREDENTIALS_PATH")
        .ok()
        .map(PathBuf::from);
    let api_key = std::env::var("DLLMD_API_KEY").ok();
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
        NetworkStore::load(&state_path, &owner_key_path)?
    } else {
        let store = NetworkStore::create(network_name);
        store.save_owner_key(&owner_key_path)?;
        store.save(&state_path)?;
        store
    };
    let mut runtime_worker = None;
    if runtime_url.is_none() {
        if let (Ok(binary), Ok(model)) = (
            std::env::var("DLLMD_RUNTIME_BIN"),
            std::env::var("DLLMD_MODEL_PATH"),
        ) {
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
            NetworkStore::load(&config.state_path, &config.owner_key_path)?
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
            },
        ));
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
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = TcpListener::bind(bind_address).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown())
            .await?;
    }
    if let Some(worker) = runtime_worker {
        worker.shutdown().await?;
    }
    Ok(())
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
