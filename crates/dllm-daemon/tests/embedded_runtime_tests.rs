//! Conformance suite for the embedded inference runtime.
//!
//! The embedded runtime and the standalone `dllm-llama-server` share the same
//! `dllm-inference` core (the `openai` chat layer, generation, embeddings,
//! tokenization, and fit), so parity between the two adapters is guaranteed by
//! construction. This suite exercises that shared core against a real model to
//! confirm the observable behavior.
//!
//! The model-backed tests are gated on the `DLLM_TEST_MODEL` environment
//! variable pointing at a local GGUF file. Without it they print a skip notice
//! and pass, so CI stays green on machines without a model. Run the full suite
//! with, for example:
//!
//! ```sh
//! DLLM_TEST_MODEL=/path/to/model.gguf cargo test -p dllm-daemon --test embedded_runtime_tests
//! ```

use dllm_daemon::embedded_runtime::{active_backend, EmbeddedRuntime};
use dllm_inference::{FitConfig, InferenceModel, InferenceParams, ModelConfig, ModelSource};
use serde_json::json;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Path to a GGUF model, or `None` to skip the model-backed tests.
fn test_model() -> Option<PathBuf> {
    std::env::var("DLLM_TEST_MODEL").ok().map(PathBuf::from)
}

/// A single model shared across all tests. The llama.cpp backend is a
/// process-global singleton, so the model is loaded exactly once (CPU only, so
/// the suite runs anywhere; accelerator coverage is in docs/gpu-native-test.md).
fn shared_runtime() -> Option<&'static EmbeddedRuntime> {
    static RUNTIME: OnceLock<Option<EmbeddedRuntime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            let path = test_model()?;
            // Offload to the accelerator when DLLM_TEST_GPU_LAYERS is set, so the
            // same suite qualifies CUDA/Vulkan builds; defaults to CPU.
            let n_gpu_layers = std::env::var("DLLM_TEST_GPU_LAYERS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            let config = ModelConfig {
                model_path: path,
                n_gpu_layers,
                ctx_size: None,
                #[cfg(feature = "mtmd")]
                mmproj: None,
            };
            let model = InferenceModel::load(config).expect("model load");
            Some(EmbeddedRuntime::new(
                model,
                1,
                active_backend(),
                "test-model".to_string(),
            ))
        })
        .as_ref()
}

/// Returns the shared runtime or prints a skip notice and returns from the test.
macro_rules! runtime_or_skip {
    () => {
        match shared_runtime() {
            Some(runtime) => runtime,
            None => {
                eprintln!("skipping: set DLLM_TEST_MODEL to a GGUF path to run this test");
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Always-on tests (no model required)
// ---------------------------------------------------------------------------

#[test]
fn error_mapping_distinguishes_client_and_server_faults() {
    let invalid = dllm_inference::InferenceError::invalid("bad input");
    let internal = dllm_inference::InferenceError::internal("boom");
    assert!(invalid.is_invalid());
    assert!(!internal.is_invalid());
}

#[test]
fn local_model_source_resolves_without_download() {
    let path = PathBuf::from("/models/example.gguf");
    let resolved = ModelSource::Local(path.clone()).resolve().unwrap();
    assert_eq!(resolved, path);
}

#[test]
fn inference_params_reject_out_of_range_values() {
    assert!(InferenceParams::from_request(&json!({ "temperature": -1.0 }), String::new()).is_err());
    assert!(InferenceParams::from_request(&json!({ "top_p": 0.0 }), String::new()).is_err());
    assert!(InferenceParams::from_request(&json!({ "max_tokens": 0 }), String::new()).is_err());
}

// ---------------------------------------------------------------------------
// Model-backed conformance (gated on DLLM_TEST_MODEL)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_blocking_returns_wellformed_completion() {
    let runtime = runtime_or_skip!();
    let response = runtime
        .chat_blocking(json!({
            "messages": [{"role": "user", "content": "Say hello in one short sentence."}],
            "max_tokens": 32,
            "temperature": 0.0,
            "stream": false,
        }))
        .await
        .expect("chat completion");

    assert_eq!(response["object"], "chat.completion");
    assert!(response["id"].as_str().unwrap().starts_with("chatcmpl-"));
    let choice = &response["choices"][0];
    assert_eq!(choice["message"]["role"], "assistant");
    assert!(!choice["message"]["content"].as_str().unwrap().is_empty());
    assert!(response["usage"]["completion_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn chat_stream_emits_role_content_finish_and_done() {
    let runtime = runtime_or_skip!();
    let mut rx = runtime
        .chat_stream(json!({
            "messages": [{"role": "user", "content": "Count to three."}],
            "max_tokens": 32,
            "temperature": 0.0,
            "stream": true,
        }))
        .await
        .expect("chat stream");

    let mut frames = Vec::new();
    while let Some(chunk) = rx.recv().await {
        frames.push(String::from_utf8(chunk).unwrap());
    }
    assert!(!frames.is_empty());
    // First data frame carries the assistant role delta.
    assert!(frames[0].contains("\"role\":\"assistant\""));
    // Stream terminates with the sentinel.
    assert_eq!(frames.last().unwrap(), "data: [DONE]\n\n");
    // Some chunk reports a finish_reason.
    assert!(frames
        .iter()
        .any(|f| f.contains("\"finish_reason\":\"stop\"")
            || f.contains("\"finish_reason\":\"length\"")));
}

#[tokio::test]
async fn chat_rejects_invalid_parameters() {
    let runtime = runtime_or_skip!();
    let err = runtime
        .chat_blocking(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": -1.0,
        }))
        .await
        .expect_err("negative temperature must be rejected");
    assert!(err.is_invalid());
}

#[test]
fn completions_generate_produces_text() {
    let model = runtime_or_skip!().engine();
    let params = InferenceParams::from_request(
        &json!({ "max_tokens": 16, "temperature": 0.0 }),
        "The capital of France is".to_string(),
    )
    .unwrap();
    let mut text = String::new();
    let (tokens, _reason) = model
        .generate(&params, |piece| {
            text.push_str(piece);
            true
        })
        .expect("generation");
    assert!(tokens > 0);
    assert!(!text.is_empty());
}

#[test]
fn embeddings_are_unit_normalized() {
    let model = runtime_or_skip!().engine();
    let vectors = model.embed(&["hello world".to_string()]).expect("embed");
    assert_eq!(vectors.len(), 1);
    assert_eq!(vectors[0].len(), model.n_embd() as usize);
    let norm: f32 = vectors[0].iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "embedding not L2-normalized: {norm}"
    );
}

#[test]
fn tokenize_detokenize_round_trips() {
    let model = runtime_or_skip!().engine();
    let pieces = model
        .tokenize("Hello, world!", false, false)
        .expect("tokenize");
    assert!(!pieces.is_empty());
    let ids: Vec<i32> = pieces.iter().map(|p| p.id).collect();
    let text = model.detokenize(&ids).expect("detokenize");
    assert!(text.contains("Hello"), "detokenized text was {text:?}");
}

#[test]
fn fit_reports_cpu_backend_and_context() {
    let path = match test_model() {
        Some(path) => path,
        None => {
            eprintln!("skipping: set DLLM_TEST_MODEL to a GGUF path to run this test");
            return;
        }
    };
    let report = dllm_inference::fit_model(&FitConfig {
        model_path: path,
        n_ctx_min: 512,
        margin_bytes: 1_073_741_824,
        backend_label: "cpu".to_string(),
    })
    .expect("fit");
    assert_eq!(report.backend, "cpu");
    assert!(report.n_ctx >= 512);
}

#[tokio::test]
async fn cancelled_stream_stops_without_panicking() {
    let runtime = runtime_or_skip!();
    let mut rx = runtime
        .chat_stream(json!({
            "messages": [{"role": "user", "content": "Write a very long story."}],
            "max_tokens": 4096,
            "temperature": 0.0,
            "stream": true,
        }))
        .await
        .expect("chat stream");

    // Consume a couple of frames, then drop the receiver. The generation loop
    // observes the closed channel and stops at the next token boundary.
    let _ = rx.recv().await;
    let _ = rx.recv().await;
    drop(rx);
    // Give the blocking task a moment to notice and exit cleanly.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn async_caller_can_bound_wait_with_timeout() {
    let runtime = runtime_or_skip!();
    // A long generation under a short timeout: the async wait returns Elapsed.
    // The underlying blocking thread keeps running to completion (llama.cpp is
    // not cancellable mid-token), which is the documented limitation.
    // Keep max_tokens modest: the blocking generation runs to completion even
    // after the async wait is abandoned, and the test runtime joins it on drop.
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        runtime.chat_blocking(json!({
            "messages": [{"role": "user", "content": "Write a very long story."}],
            "max_tokens": 96,
            "temperature": 0.0,
        })),
    )
    .await;
    assert!(result.is_err(), "expected the bounded wait to time out");
}
