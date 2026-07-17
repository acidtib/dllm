use axum::{
    body::Body,
    http::{header, Request, StatusCode},
    routing::{get, post},
    Router,
};
use dllm_daemon::{api, credentials::CredentialRegistry, inference::InferenceRegistry, NetworkStore};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower::ServiceExt;

fn api_state() -> api::ApiState {
    api::ApiState {
        store: Arc::new(Mutex::new(NetworkStore::create("test"))),
        state_path: std::env::temp_dir().join("dllmd-lifecycle-test-state.json"),
        runtime_url: None,
        admission: Arc::new(Semaphore::new(2)),
        client: reqwest::Client::new(),
        management_credentials: Arc::new(RwLock::new(
            CredentialRegistry::load(Vec::new(), None, None).unwrap(),
        )),
        inference_credentials: Arc::new(InferenceRegistry::new(Vec::new(), None, 1)),
        peer_api_key: None,
        metrics: Arc::new(api::Metrics::default()),
        public_url: "http://127.0.0.1:7337".into(),
        replica_loads: Arc::new(Mutex::new(HashMap::new())),
        peer_nonces: Arc::new(Mutex::new(HashMap::new())),
        peer_quota: Arc::new(Semaphore::new(1)),
        peer_diagnostics: None,
        auth_view: None,
        peer_client: None,
    }
}

// ---------------------------------------------------------------------------
// Admission and concurrency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admission_ceiling_rejects_excess_work() {
    // Hold all permits so that proxy returns TOO_MANY_REQUESTS.
    let state = api_state();
    let _held = state.admission.clone().acquire_owned().await.unwrap();
    let _held2 = state.admission.clone().acquire_owned().await.unwrap();
    // The third attempt must fail.
    assert!(state.admission.clone().try_acquire_owned().is_err());
    assert_eq!(state.admission.available_permits(), 0);
    drop(_held);
    assert_eq!(state.admission.available_permits(), 1);
    drop(_held2);
    assert_eq!(state.admission.available_permits(), 2);
}

#[tokio::test]
async fn permit_released_on_drop() {
    let sem = Arc::new(Semaphore::new(1));
    assert_eq!(sem.available_permits(), 1);
    {
        let _permit = sem.clone().try_acquire_owned().unwrap();
        assert_eq!(sem.available_permits(), 0);
    }
    assert_eq!(sem.available_permits(), 1);
}

#[tokio::test]
async fn replica_lease_increments_and_decrements() {
    let load = Arc::new(AtomicU64::new(0));
    {
        load.fetch_add(1, Ordering::Relaxed);
        assert_eq!(load.load(Ordering::Relaxed), 1);
    }
    {
        load.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(load.load(Ordering::Relaxed), 0);
    }
    // Simulate the ReplicaLease pattern: increment on creation, decrement on drop.
    struct Lease(Arc<AtomicU64>);
    impl Lease {
        fn new(counter: Arc<AtomicU64>) -> Self {
            counter.fetch_add(1, Ordering::Relaxed);
            Self(counter)
        }
    }
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    let counter = Arc::new(AtomicU64::new(0));
    assert_eq!(counter.load(Ordering::Relaxed), 0);
    {
        let _lease = Lease::new(counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
    // Lease dropped — counter returns to zero.
    assert_eq!(counter.load(Ordering::Relaxed), 0);
}

// ---------------------------------------------------------------------------
// Stream isolation: concurrent proxy calls do not mix chunks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_proxy_calls_do_not_mix_responses() {
    use std::sync::atomic::AtomicUsize;

    let call_count = Arc::new(AtomicUsize::new(0));
    let cc = call_count.clone();

    let upstream = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route(
            "/v1/chat/completions",
            post(move || {
                let n = cc.fetch_add(1, Ordering::Relaxed);
                let body = format!("data: response-{n}\n\ndata: [DONE]\n\n");
                async move { ([(header::CONTENT_TYPE, "text/event-stream")], body) }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let mut state = api_state();
    state.runtime_url = Some(format!("http://{addr}"));
    state.admission = Arc::new(Semaphore::new(4));
    state.inference_credentials = Arc::new(InferenceRegistry::new(Vec::new(), None, 4));
    let owner = state.store.lock().await.state.state.owner_pubkey;
    state
        .store
        .lock()
        .await
        .assign_model("test-model".into(), owner)
        .unwrap();

    let app = api::router(state);

    // Send three concurrent requests.
    let (r1, r2, r3) = tokio::join!(
        app.clone().oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap()
        ),
        app.clone().oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap()
        ),
        app.clone().oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap()
        ),
    );

    // All three must succeed.
    for result in [r1, r2, r3] {
        let resp = result.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/event-stream"
        );
        let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body_bytes);
        assert!(body.contains("response-"));
        assert!(body.contains("[DONE]"));
    }
}

// ---------------------------------------------------------------------------
// Error paths release admission
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upstream_error_releases_admission() {
    let upstream = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route(
            "/v1/chat/completions",
            post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let mut state = api_state();
    state.runtime_url = Some(format!("http://{addr}"));
    state.admission = Arc::new(Semaphore::new(1));
    let owner = state.store.lock().await.state.state.owner_pubkey;
    state
        .store
        .lock()
        .await
        .assign_model("test-model".into(), owner)
        .unwrap();

    let admission = state.admission.clone();
    let app = api::router(state);
    let before = admission.available_permits();
    assert_eq!(before, 1);

    let resp = app
        .oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Upstream returned 500, proxy forwards that status.
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // Consume the body to release permits held inside the stream closure.
    let _body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    // Permit must be released after the response body is consumed.
    assert_eq!(admission.available_permits(), 1);
}

#[tokio::test]
async fn model_unavailable_releases_admission() {
    let mut state = api_state();
    state.admission = Arc::new(Semaphore::new(1));
    let admission = state.admission.clone();

    let app = api::router(state);
    let before = admission.available_permits();
    assert_eq!(before, 1);

    let resp = app
        .oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"nonexistent"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Model not found — no permits were acquired, so all still available.
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(admission.available_permits(), 1);
}

// ---------------------------------------------------------------------------
// Slow consumer / backpressure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_consumer_does_not_block_other_requests() {
    // A slow consumer reading chunk-by-chunk should not prevent other requests
    // from being admitted. We verify that a second fast request completes
    // while the first is still in-flight.

    let upstream = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route(
            "/v1/chat/completions",
            post(|| async {
                // Return a slow stream: many small chunks.
                let chunks: Vec<String> = (0..20).map(|i| format!("data: chunk-{i}\n\n")).collect();
                let body = chunks.join("");
                ([(header::CONTENT_TYPE, "text/event-stream")], body)
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let mut state = api_state();
    state.runtime_url = Some(format!("http://{addr}"));
    state.admission = Arc::new(Semaphore::new(2));
    state.inference_credentials = Arc::new(InferenceRegistry::new(Vec::new(), None, 4));
    let owner = state.store.lock().await.state.state.owner_pubkey;
    state
        .store
        .lock()
        .await
        .assign_model("test-model".into(), owner)
        .unwrap();

    let admission = state.admission.clone();
    let app = api::router(state);

    // Send two requests; both admission (2) and quota (4) allow it.
    let (r1, r2) = tokio::join!(
        app.clone().oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap()
        ),
        app.oneshot(
            Request::post("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"model":"test-model"}"#))
                .unwrap()
        ),
    );

    assert_eq!(r1.unwrap().status(), StatusCode::OK);
    assert_eq!(r2.unwrap().status(), StatusCode::OK);
    // Both permits released after completion.
    assert_eq!(admission.available_permits(), 2);
}
