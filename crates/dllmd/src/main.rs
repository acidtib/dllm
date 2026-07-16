use dllmd::{api, NetworkStore};
use std::{path::PathBuf, sync::Arc};
use tokio::{net::TcpListener, sync::Mutex};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("DLLMD_BIND").unwrap_or_else(|_| "127.0.0.1:7337".into());
    let state_path =
        PathBuf::from(std::env::var("DLLMD_STATE").unwrap_or_else(|_| "dllm-state.json".into()));
    let network_name = std::env::var("DLLMD_NETWORK").unwrap_or_else(|_| "private".into());
    let store = NetworkStore::create(network_name);
    store.save(&state_path)?;
    let app = api::router(api::ApiState {
        store: Arc::new(Mutex::new(store)),
        state_path,
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
