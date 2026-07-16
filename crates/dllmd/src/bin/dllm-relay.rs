use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use futures_util::StreamExt;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;

#[derive(Clone)]
struct RelayState {
    upstream: String,
    client: reqwest::Client,
    delay: Duration,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("DLLM_RELAY_BIND").unwrap_or_else(|_| "127.0.0.1:7443".into());
    let upstream = std::env::var("DLLM_RELAY_UPSTREAM")?;
    let delay = Duration::from_millis(
        std::env::var("DLLM_RELAY_DELAY_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    );
    let state = Arc::new(RelayState {
        upstream: upstream.trim_end_matches('/').to_owned(),
        client: reqwest::Client::new(),
        delay,
    });
    let app = Router::new()
        .route("/v1/peer/{*path}", any(forward_peer))
        .with_state(state);
    let address: SocketAddr = bind.parse()?;
    println!("dllm-relay listening on {address}");
    axum::serve(TcpListener::bind(address).await?, app).await?;
    Ok(())
}

async fn forward_peer(
    State(state): State<Arc<RelayState>>,
    request: Request,
) -> Result<Response, StatusCode> {
    if state.delay != Duration::ZERO {
        tokio::time::sleep(state.delay).await;
    }
    let path = request
        .uri()
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or(request.uri().path())
        .to_owned();
    let method = request.method().clone();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut upstream = state
        .client
        .request(method, format!("{}{path}", state.upstream));
    for name in [
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
        header::ACCEPT,
        header::HeaderName::from_static("x-dllm-network-id"),
        header::HeaderName::from_static("x-dllm-node-key"),
        header::HeaderName::from_static("x-dllm-timestamp"),
        header::HeaderName::from_static("x-dllm-nonce"),
        header::HeaderName::from_static("x-dllm-signature"),
    ] {
        if let Some(value) = headers.get(&name) {
            upstream = upstream.header(name, value);
        }
    }
    if !body.is_empty() {
        upstream = upstream.body(body);
    }
    let response = upstream.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let status = response.status();
    let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
    let stream = response
        .bytes_stream()
        .map(|item| item.map_err(std::io::Error::other));
    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(Body::from_stream(stream))
        .map_err(|_| StatusCode::BAD_GATEWAY)
}
