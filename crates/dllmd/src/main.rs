use dllmd::{api, NetworkStore};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Semaphore},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("DLLMD_BIND").unwrap_or_else(|_| "127.0.0.1:7337".into());
    let state_path =
        PathBuf::from(std::env::var("DLLMD_STATE").unwrap_or_else(|_| "dllm-state.json".into()));
    let owner_key_path =
        PathBuf::from(std::env::var("DLLMD_OWNER_KEY").unwrap_or_else(|_| "dllm-owner.key".into()));
    let network_name = std::env::var("DLLMD_NETWORK").unwrap_or_else(|_| "private".into());
    let runtime_url = std::env::var("DLLMD_RUNTIME_URL").ok();
    let admission_limit = std::env::var("DLLMD_ADMISSION_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);
    let store = if state_path.exists() {
        NetworkStore::load(&state_path, &owner_key_path)?
    } else {
        let store = NetworkStore::create(network_name);
        store.save_owner_key(&owner_key_path)?;
        store.save(&state_path)?;
        store
    };
    let app = api::router(api::ApiState {
        store: Arc::new(Mutex::new(store)),
        state_path,
        runtime_url,
        admission: Arc::new(Semaphore::new(admission_limit)),
        client: reqwest::Client::new(),
    });
    let listener = TcpListener::bind(&bind).await?;
    println!("dllmd listening on {bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
