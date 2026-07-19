//! In-process inference runtime for `dllmd`.
//!
//! Replaces the bundled `dllm-llama-server` child process: the daemon loads a
//! model with `dllm-inference` and serves local chat completions by direct
//! function call. Peer forwarding and the external `DLLMD_RUNTIME_URL` adapter
//! still use HTTP.
//!
//! llama.cpp contexts are not thread-safe, so generation is serialized through
//! a semaphore and always runs on a blocking thread. A native crash inside
//! llama.cpp terminates the process; this runtime does not claim to recover
//! from that.

use dllm_inference::{openai, InferenceError, InferenceModel};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Semaphore};

/// Prompt used to measure prefill and decode throughput for the hardware
/// benchmark. Matches the previous HTTP benchmark so numbers stay comparable.
const BENCHMARK_PROMPT: &str = "The quick brown fox jumps over the lazy dog. Describe what \
    happens next in exactly one sentence, staying factual and concise.";

/// Backend the binary was built for. Runtime backend discovery (the ggml probe)
/// replaces this once it lands; today the binary reports its compile-time
/// backend so the hardware profile records something honest.
pub fn active_backend() -> &'static str {
    if cfg!(feature = "cuda") {
        "cuda"
    } else if cfg!(feature = "vulkan") {
        "vulkan"
    } else if cfg!(feature = "metal") {
        "metal"
    } else {
        "cpu"
    }
}

/// A loaded model served in-process. Cheap to clone (holds `Arc`s).
#[derive(Clone)]
pub struct EmbeddedRuntime {
    engine: Arc<InferenceModel>,
    /// Serializes generation: llama.cpp contexts are not thread-safe.
    gen_semaphore: Arc<Semaphore>,
    backend: &'static str,
    model_label: String,
}

impl EmbeddedRuntime {
    pub fn new(
        engine: InferenceModel,
        parallel: usize,
        backend: &'static str,
        model_label: String,
    ) -> Self {
        Self {
            engine: Arc::new(engine),
            gen_semaphore: Arc::new(Semaphore::new(parallel.max(1))),
            backend,
            model_label,
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend
    }

    pub fn model_label(&self) -> &str {
        &self.model_label
    }

    /// Prepare a chat request (validation, prompt render) and resolve any image
    /// sources to bytes. Runs off the generation semaphore since it does no
    /// llama.cpp compute beyond prompt rendering.
    async fn prepare(&self, req: &Value) -> Result<openai::PreparedChat, InferenceError> {
        #[allow(unused_mut)]
        let mut prepared = openai::prepare_chat(&self.engine, req)?;
        #[cfg(feature = "mtmd")]
        if !prepared.image_sources.is_empty() {
            let sources = std::mem::take(&mut prepared.image_sources);
            let mut bytes = Vec::with_capacity(sources.len());
            for source in sources {
                match source {
                    openai::ImageSource::Url(url) => {
                        bytes.push(openai::fetch_image_url(&url).await?);
                    }
                    openai::ImageSource::FileId(id) => {
                        return Err(InferenceError::invalid(format!(
                            "file '{id}' is not available: the embedded runtime has no file store"
                        )));
                    }
                }
            }
            prepared.params.image_bytes = bytes;
        }
        Ok(prepared)
    }

    /// Non-streaming chat completion. Returns the OpenAI `chat.completion`
    /// response value.
    pub async fn chat_blocking(&self, req: Value) -> Result<Value, InferenceError> {
        let prepared = self.prepare(&req).await?;
        let permit = self
            .gen_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("generation semaphore is never closed");
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            openai::chat_blocking(&engine, &prepared)
        })
        .await
        .map_err(|e| InferenceError::internal(format!("inference task panicked: {e}")))?
    }

    /// Streaming chat completion. Returns a receiver of pre-framed SSE byte
    /// chunks (`data: {...}\n\n`), terminated by `data: [DONE]\n\n`. Errors that
    /// occur during preparation surface here before any bytes are streamed.
    pub async fn chat_stream(&self, req: Value) -> Result<mpsc::Receiver<Vec<u8>>, InferenceError> {
        let prepared = self.prepare(&req).await?;
        let permit = self
            .gen_semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("generation semaphore is never closed");
        let engine = self.engine.clone();
        let (tx, rx) = mpsc::channel::<Vec<u8>>(32);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            openai::chat_stream(&engine, &prepared, |chunk| {
                tx.blocking_send(sse_frame(&chunk)).is_ok()
            });
            let _ = tx.blocking_send(sse_done_frame());
        });
        Ok(rx)
    }

    /// Measure prefill and decode throughput (milli-tokens per second) for the
    /// hardware benchmark, driving the model directly instead of over HTTP.
    pub async fn measure_throughput(&self) -> Result<MeasuredThroughput, InferenceError> {
        let engine = self.engine.clone();
        let prompt_tokens = tokio::task::spawn_blocking(move || {
            engine
                .tokenize(BENCHMARK_PROMPT, false, false)
                .map(|t| t.len() as u64)
        })
        .await
        .map_err(|e| InferenceError::internal(format!("tokenize task panicked: {e}")))??;

        let prefill_start = Instant::now();
        self.chat_blocking(json!({
            "messages": [{"role": "user", "content": BENCHMARK_PROMPT}],
            "max_tokens": 1,
            "stream": false,
        }))
        .await?;
        let prefill_elapsed = prefill_start.elapsed().as_secs_f64();

        let decode_start = Instant::now();
        let decode_response = self
            .chat_blocking(json!({
                "messages": [{"role": "user", "content": "Count from one to twenty."}],
                "max_tokens": 64,
                "stream": false,
            }))
            .await?;
        let decode_elapsed = decode_start.elapsed().as_secs_f64();
        let completion_tokens = decode_response["usage"]["completion_tokens"]
            .as_u64()
            .unwrap_or(0);

        Ok(MeasuredThroughput {
            prompt_tokens_per_second_milli: rate_milli(prompt_tokens, prefill_elapsed),
            decode_tokens_per_second_milli: rate_milli(completion_tokens, decode_elapsed),
        })
    }
}

pub struct MeasuredThroughput {
    pub prompt_tokens_per_second_milli: u64,
    pub decode_tokens_per_second_milli: u64,
}

fn rate_milli(tokens: u64, elapsed_secs: f64) -> u64 {
    if elapsed_secs > 0.0 {
        (tokens as f64 / elapsed_secs * 1000.0) as u64
    } else {
        0
    }
}

fn sse_frame(chunk: &Value) -> Vec<u8> {
    format!("data: {chunk}\n\n").into_bytes()
}

fn sse_done_frame() -> Vec<u8> {
    b"data: [DONE]\n\n".to_vec()
}
