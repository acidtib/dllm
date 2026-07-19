//! OpenAI-compatible chat/completion/embedding HTTP server.
//!
//! This binary is a thin HTTP adapter over the `dllm-inference` library, which
//! owns all llama.cpp state and generation. The server handles request parsing,
//! auth, streaming, the OpenAI protocol shaping (tool calls, message
//! normalization), and the uploaded-file store.
//!
//! # Endpoints
//!
//! | Method | Path                    | Description                     |
//! |--------|-------------------------|---------------------------------|
//! | GET    | `/health`               | Liveness check                  |
//! | GET    | `/v1/models`            | List loaded model                |
//! | POST   | `/v1/chat/completions`  | Chat (streaming + non-streaming) |
//! | POST   | `/v1/completions`       | Raw text completion (streaming)  |
//! | POST   | `/v1/embeddings`        | Dense embeddings                 |
//! | POST   | `/v1/files`             | Upload files (multimodal, mtmd)  |
//! | POST   | `/tokenize`             | Tokenize text (llama.cpp compat) |
//! | POST   | `/detokenize`           | Detokenize token ids             |
//!
//! Legacy paths without `/v1` (`/completions`, `/embeddings`, `/chat/completions`)
//! are also registered for llama.cpp server compatibility.
//!
//! # Usage
//!
//! ```console
//! # Local file
//! cargo run -p dllm-llama-server -- local path/to/model.gguf
//!
//! # Hugging Face (interactive quant picker)
//! cargo run -p dllm-llama-server -- hf-model unsloth/Qwen3.5-397B-A17B-GGUF
//!
//! # With GPU + auth key
//! cargo run -p dllm-llama-server --features metal -- \
//!     --n-gpu-layers 99 --api-key secret \
//!     hf-model bartowski/Llama-3.2-3B-Instruct-GGUF Q4_K_M
//! ```
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::case_sensitive_file_extension_comparisons
)]

mod tools;

use actix_multipart::Multipart;
use actix_web::{http::StatusCode, web, App, HttpRequest, HttpResponse, HttpServer};
use clap::Parser;
use dllm_inference::{InferenceError, InferenceModel, InferenceParams, ModelConfig, ModelSource};
use futures_util::{stream, StreamExt as _};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, RwLock, Semaphore};
use tools::{extract_tool_calls, inject_tools, normalise_messages, parse_tool_choice, parse_tools};
#[cfg(feature = "mtmd")]
use tools::{normalise_messages_multimodal, ImageSource};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "dllm-llama-server",
    about = "OpenAI-compatible llama.cpp server"
)]
struct Args {
    /// Host to listen on.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Number of layers to offload to GPU (0 = CPU only).
    #[arg(long, default_value_t = 0)]
    n_gpu_layers: u32,

    /// Context size override (default: use the model's trained context length).
    #[arg(short = 'c', long)]
    ctx_size: Option<NonZeroU32>,

    /// Require this bearer token on every request. Disabled when omitted.
    #[arg(long)]
    api_key: Option<String>,

    /// Maximum number of requests processed concurrently.
    /// llama.cpp contexts are not thread-safe so this effectively serialises
    /// inference while keeping HTTP connections responsive.
    #[arg(long, default_value_t = 1)]
    parallel: usize,

    /// Resolve (and download) the model, print its absolute local path to
    /// stdout, then exit without starting the server.
    #[arg(long)]
    print_path: bool,

    /// Compute a fitted n_gpu_layers/n_ctx for the resolved model against
    /// available device memory, print the result as JSON, then exit without
    /// starting the server.
    #[arg(long)]
    fit: bool,

    /// Minimum context size the fit calculation will keep when memory is tight.
    #[arg(long, default_value_t = 4096)]
    fit_n_ctx_min: u32,

    /// Minimum free memory (bytes) to leave on each device after fitting.
    #[arg(long, default_value_t = 1_073_741_824)]
    fit_margin_bytes: usize,

    // ── Multimodal (mtmd) ──────────────────────────────────────────────────
    /// Path to the multimodal projector (mmproj) GGUF file.
    /// Enables the `POST /v1/files` endpoint and image/audio inputs in chat
    /// completions.  Requires the `mtmd` Cargo feature.
    #[arg(long, value_name = "FILE")]
    mmproj: Option<PathBuf>,

    /// Number of threads used by the vision/audio encoder (default: 4).
    #[arg(long, default_value_t = 4)]
    mmproj_n_threads: i32,

    /// Do NOT offload the mmproj model to the GPU.
    #[arg(long)]
    no_mmproj_gpu: bool,

    #[command(subcommand)]
    model: ModelArg,
}

#[derive(clap::Subcommand, Debug)]
enum ModelArg {
    /// Load a model from a local file path.
    Local {
        /// Path to the GGUF model file.
        path: PathBuf,
    },
    /// Download a model from Hugging Face Hub (cached locally).
    ///
    /// If `<model>` is omitted the repo's GGUF files are listed and you are
    /// prompted to choose interactively (best quant auto-picked when stdin is
    /// not a terminal).  For sharded repos all shards are downloaded.
    #[clap(name = "hf-model")]
    HuggingFace {
        /// Repository id, e.g. `unsloth/Qwen3.5-397B-A17B-GGUF`.
        repo: String,
        /// Exact filename or quant directory name (e.g. `Q4_K_M`).
        /// Omit to pick interactively.
        model: Option<String>,
    },
}

impl ModelArg {
    fn into_source(self) -> ModelSource {
        match self {
            ModelArg::Local { path } => ModelSource::Local(path),
            ModelArg::HuggingFace { repo, model } => ModelSource::HuggingFace { repo, model },
        }
    }
}

/// Compile-time backend label. Backend selection becomes runtime discovery in
/// `dllmd`; this binary still reports the accelerator it was built for.
fn active_backend() -> &'static str {
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

// ---------------------------------------------------------------------------
// File store
// ---------------------------------------------------------------------------

/// A file uploaded via `POST /v1/files`.
#[derive(Debug, Clone)]
struct FileEntry {
    id: String,
    filename: String,
    bytes: Vec<u8>,
    purpose: String,
    created_at: u64,
}

/// Generate a stable file ID by FNV-1a hashing the content + timestamp.
fn gen_file_id(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325_u64;
    for &b in data {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    for &b in &now_secs().to_le_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("file-{h:016x}")
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

struct AppState {
    engine: InferenceModel,
    /// Limits the number of concurrent inference calls.
    inference_semaphore: Arc<Semaphore>,
    /// Optional bearer token that every request must present.
    api_key: Option<String>,
    /// In-memory store for files uploaded via `POST /v1/files`.
    file_store: Arc<RwLock<HashMap<String, FileEntry>>>,
}

// ---------------------------------------------------------------------------
// HTTP error helpers
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct HttpError {
    status: StatusCode,
    r#type: &'static str,
    message: String,
}

fn bad_request(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::BAD_REQUEST,
        r#type: "invalid_request_error",
        message: msg.into(),
    }
}

fn unauthorized(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::UNAUTHORIZED,
        r#type: "authentication_error",
        message: msg.into(),
    }
}

fn internal_error(msg: impl Into<String>) -> HttpError {
    HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        r#type: "server_error",
        message: msg.into(),
    }
}

/// Map a library error onto the matching HTTP status: a caller mistake is 400,
/// an internal failure is 500.
fn http_from(err: InferenceError) -> HttpError {
    if err.is_invalid() {
        bad_request(err.to_string())
    } else {
        internal_error(err.to_string())
    }
}

fn error_response(err: HttpError) -> HttpResponse {
    let body = json!({
        "error": {
            "message": err.message,
            "type": err.r#type,
            "code": err.status.as_u16()
        }
    })
    .to_string();
    HttpResponse::build(err.status)
        .content_type("application/json")
        .body(body)
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

fn check_auth(req: &HttpRequest, state: &AppState) -> Option<HttpError> {
    let expected = state.api_key.as_ref()?;
    let auth = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());
    match auth {
        Some(v) if v == format!("Bearer {expected}") => None,
        _ => Some(unauthorized("invalid or missing API key")),
    }
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

/// `OpenAI` uses `max_tokens`; newer clients may send `max_completion_tokens`.
/// Used for the early sampling-parameter gate before prompt rendering.
fn parse_max_tokens(req: &Value) -> Result<u32, HttpError> {
    let raw = req
        .get("max_completion_tokens")
        .or_else(|| req.get("max_tokens"));
    match raw {
        None | Some(Value::Null) => Ok(1024),
        Some(v) => {
            let n = v
                .as_u64()
                .ok_or_else(|| bad_request("'max_tokens' must be a positive integer"))?;
            if n == 0 {
                return Err(bad_request("'max_tokens' must be > 0"));
            }
            u32::try_from(n).map_err(|_| bad_request("'max_tokens' is too large"))
        }
    }
}

// ---------------------------------------------------------------------------
// Multimodal helpers (compiled only when the `mtmd` feature is active)
// ---------------------------------------------------------------------------

/// Decode a `data:` URI or fetch an `http(s)://` URL, returning raw bytes.
#[cfg(feature = "mtmd")]
async fn fetch_url_bytes(url: &str) -> Result<Vec<u8>, HttpError> {
    tracing::info!("Fetching image: {}…", &url[..url.len().min(120)]);
    if let Some(rest) = url.strip_prefix("data:") {
        // data:[<mediatype>][;base64],<data>
        let comma = rest
            .find(',')
            .ok_or_else(|| bad_request("invalid data URI: missing ','"))?;
        let meta = &rest[..comma];
        let data = &rest[comma + 1..];
        if meta.ends_with(";base64") {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| bad_request(format!("base64 decode error: {e}")))?;
            tracing::info!("Decoded {} bytes from data URI", bytes.len());
            Ok(bytes)
        } else {
            // Plain text / URL-encoded — treat the raw bytes as the payload.
            Ok(data.as_bytes().to_vec())
        }
    } else if url.starts_with("http://") || url.starts_with("https://") {
        // Many CDNs (including Wikimedia) block requests that lack a
        // browser-like User-Agent and return an HTML error page instead of the
        // image.  stb_image then fails because it receives HTML, not JPEG/PNG.
        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (compatible; llama-cpp-rs; \
                 +https://github.com/utilityai/llama-cpp-rs)",
            )
            .build()
            .map_err(|e| internal_error(format!("reqwest client: {e}")))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| bad_request(format!("failed to fetch image URL: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(bad_request(format!(
                "image URL returned HTTP {status}: {url}"
            )));
        }

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_owned();

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| bad_request(format!("failed to read image response: {e}")))?;

        tracing::info!(
            "Downloaded {} bytes (content-type: {content_type}) from URL",
            bytes.len()
        );

        // Anything under 1 KB cannot be a real image — print the body so the
        // user can see what the server actually returned (redirect HTML, JSON
        // error, Cloudflare challenge, etc.).
        if bytes.len() < 1024 {
            let preview = std::str::from_utf8(&bytes).unwrap_or("(binary)");
            return Err(bad_request(format!(
                "image URL returned only {} bytes — not a valid image file. \
                 Response body: {preview:?}",
                bytes.len()
            )));
        }

        // Warn if the response looks like HTML rather than binary image data.
        // JPEG magic = 0xFF 0xD8; PNG = 0x89 0x50 0x4E; GIF = 0x47 0x49 0x46.
        if bytes.starts_with(b"<!") || bytes.starts_with(b"<h") || bytes.starts_with(b"<H") {
            return Err(bad_request(format!(
                "image URL returned HTML instead of an image. \
                 The server likely rejected the request (check the URL and any auth). \
                 First 200 bytes: {:?}",
                std::str::from_utf8(&bytes[..bytes.len().min(200)]).unwrap_or("(invalid utf-8)")
            )));
        }

        Ok(bytes.to_vec())
    } else {
        Err(bad_request(
            "unsupported image source: must start with 'data:', 'http://', or 'https://'",
        ))
    }
}

/// Resolve a list of [`ImageSource`] items to raw byte vectors.
/// `FileId` sources are looked up in the shared file store;
/// `Url` sources are decoded / fetched from the network.
#[cfg(feature = "mtmd")]
async fn resolve_image_sources(
    sources: Vec<ImageSource>,
    file_store: &RwLock<HashMap<String, FileEntry>>,
) -> Result<Vec<Vec<u8>>, HttpError> {
    let mut out = Vec::with_capacity(sources.len());
    for src in sources {
        let bytes = match src {
            ImageSource::Url(url) => fetch_url_bytes(&url).await?,
            ImageSource::FileId(id) => {
                let store = file_store.read().await;
                store.get(&id).map(|e| e.bytes.clone()).ok_or_else(|| {
                    bad_request(format!(
                        "file '{id}' not found — upload it first via POST /v1/files"
                    ))
                })?
            }
        };
        out.push(bytes);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// SSE helpers
// ---------------------------------------------------------------------------

fn sse_chunk(data: &Value) -> web::Bytes {
    web::Bytes::from(format!("data: {data}\n\n"))
}

fn sse_done() -> web::Bytes {
    web::Bytes::from("data: [DONE]\n\n")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Chat completions  POST /v1/chat/completions
// ---------------------------------------------------------------------------

async fn chat_completions(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Bytes,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let text = match std::str::from_utf8(&body) {
        Ok(s) => s.to_owned(),
        Err(_) => return error_response(bad_request("body must be valid UTF-8")),
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return error_response(bad_request(format!("invalid JSON: {e}"))),
    };

    let streaming = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // ── Early parameter validation ────────────────────────────────────────────
    // Validate sampling params *before* the expensive apply_chat_template call.
    // This guarantees that invalid values (e.g. temperature < 0) return the
    // correct 400 response rather than a 500 from a later failure.
    {
        let temperature = parsed
            .get("temperature")
            .and_then(Value::as_f64)
            .unwrap_or(1.0) as f32;
        if temperature < 0.0 {
            return error_response(bad_request("'temperature' must be >= 0"));
        }
        let top_p = parsed.get("top_p").and_then(Value::as_f64).unwrap_or(1.0) as f32;
        if !(0.0 < top_p && top_p <= 1.0) {
            return error_response(bad_request("'top_p' must be in (0, 1]"));
        }
        let top_k = parsed.get("top_k").and_then(Value::as_i64).unwrap_or(0) as i32;
        if top_k < 0 {
            return error_response(bad_request("'top_k' must be >= 0"));
        }
        if let Err(e) = parse_max_tokens(&parsed) {
            return error_response(e);
        }
        if matches!(
            parsed.get("grammar"),
            Some(v) if !v.is_string() && !v.is_null()
        ) {
            return error_response(bad_request("'grammar' must be a GBNF string"));
        }
    }

    // ── Parse tools ──────────────────────────────────────────────────────────
    let tool_defs = match parse_tools(&parsed) {
        Ok(t) => t,
        Err(e) => return error_response(e),
    };
    let tool_choice = match parse_tool_choice(&parsed) {
        Ok(c) => c,
        Err(e) => return error_response(e),
    };

    // ── Parse messages (with multimodal support when available) ─────────────
    // Always run the multimodal parser so we can count image parts, even when
    // there is no projector — the count is used only for the warning.
    #[cfg(feature = "mtmd")]
    let (base_msg_pairs, image_sources) = {
        let marker = dllm_inference::mtmd_default_marker();
        tracing::debug!("mtmd media marker: {:?}", marker);
        let (pairs, sources) = match normalise_messages_multimodal(&parsed, &marker) {
            Ok(r) => r,
            Err(e) => return error_response(e),
        };
        if !sources.is_empty() {
            tracing::info!(
                n_images = sources.len(),
                mtmd_ctx_present = state.engine.has_mtmd(),
                "Detected multimodal content in request"
            );
        }
        if !sources.is_empty() && !state.engine.has_mtmd() {
            tracing::warn!(
                n_images = sources.len(),
                "Request contains image(s) but the server was started without --mmproj. \
                 Images will be IGNORED and the prompt processed as plain text. \
                 Restart with `--mmproj <path-to-mmproj.gguf>` and a vision-capable model \
                 to enable multimodal inference."
            );
            // Fall back to the text-only normaliser so markers are not left in the prompt.
            match normalise_messages(&parsed) {
                Ok(text_pairs) => (text_pairs, vec![]),
                Err(e) => return error_response(e),
            }
        } else {
            (pairs, sources)
        }
    };

    #[cfg(not(feature = "mtmd"))]
    let base_msg_pairs = match normalise_messages(&parsed) {
        Ok(m) => m,
        Err(e) => return error_response(e),
    };

    // ── Build prompt from messages ───────────────────────────────────────────
    let prompt = {
        let mut msg_pairs = base_msg_pairs;

        // Inject tool definitions + usage instructions into the system message.
        inject_tools(&mut msg_pairs, &tool_defs, &tool_choice);

        let template_override = match parsed.get("chat_template") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            _ => return error_response(bad_request("'chat_template' must be a string")),
        };
        match state
            .engine
            .render_prompt(template_override.as_deref(), &msg_pairs)
        {
            Ok(p) => p,
            Err(e) => return error_response(http_from(e)),
        }
    };

    // ── Sampling params ───────────────────────────────────────────────────────
    let mut params = match InferenceParams::from_request(&parsed, prompt) {
        Ok(p) => p,
        Err(e) => return error_response(http_from(e)),
    };

    // ── Resolve image sources → raw bytes (mtmd path only) ───────────────────
    #[cfg(feature = "mtmd")]
    if !image_sources.is_empty() {
        tracing::info!("Resolving {} image source(s)…", image_sources.len());
        match resolve_image_sources(image_sources, &state.file_store).await {
            Ok(bytes) => {
                tracing::info!(
                    "Images ready: {} image(s), sizes: {:?}",
                    bytes.len(),
                    bytes.iter().map(|b| b.len()).collect::<Vec<_>>()
                );
                params.image_bytes = bytes;
            }
            Err(e) => return error_response(e),
        }
    }

    // When tools are in play, give the model enough room to think and then emit
    // a complete tool call (thinking models need extra tokens for their
    // reasoning block before the tool call). Grammar-based forcing is
    // intentionally NOT used: GBNF grammars conflict with special tokens such
    // as <tool_call> and prevent thinking models from emitting reasoning. The
    // system-prompt injection is sufficient for capable models.
    if !tool_defs.is_empty() && params.max_tokens < 1024 {
        params.max_tokens = 1024;
    }

    let model_name = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(state.engine.model_name())
        .to_owned();
    let has_tools = !tool_defs.is_empty();
    let created = now_secs();
    let id = format!("chatcmpl-{created}");

    if streaming {
        run_chat_stream(state, params, id, model_name, created, has_tools).await
    } else {
        run_chat_blocking(state, params, id, model_name, created, has_tools).await
    }
}

async fn run_chat_blocking(
    state: web::Data<AppState>,
    params: InferenceParams,
    id: String,
    model_name: String,
    created: u64,
    has_tools: bool,
) -> HttpResponse {
    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut raw = String::new();
        let outcome = state2.engine.generate(&params, |piece| {
            raw.push_str(piece);
            true
        });
        outcome.map(|(tokens, reason)| (raw, tokens, reason))
    })
    .await;

    match result {
        Ok(Ok((raw_output, completion_tokens, finish_reason))) => {
            let prompt_tokens = 0u32; // cheap approximation; full count needs a 2nd tokenise pass

            // Parse tool calls out of the raw output.
            let (content, tool_calls) = if has_tools {
                extract_tool_calls(&raw_output)
            } else {
                (raw_output, vec![])
            };

            let (final_finish, message) = if tool_calls.is_empty() {
                (
                    finish_reason.as_str(),
                    json!({ "role": "assistant", "content": content }),
                )
            } else {
                let calls_json: Vec<Value> =
                    tool_calls.iter().map(tools::ToolCall::to_value).collect();
                (
                    "tool_calls",
                    json!({
                        "role": "assistant",
                        "content": if content.is_empty() { Value::Null } else { Value::String(content) },
                        "tool_calls": calls_json
                    }),
                )
            };

            HttpResponse::Ok().content_type("application/json").body(
                json!({
                    "id": id,
                    "object": "chat.completion",
                    "created": created,
                    "model": model_name,
                    "choices": [{"index": 0, "message": message, "finish_reason": final_finish}],
                    "usage": {
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": prompt_tokens + completion_tokens
                    }
                })
                .to_string(),
            )
        }
        Ok(Err(e)) => error_response(http_from(e)),
        Err(e) => error_response(internal_error(format!("inference task panicked: {e}"))),
    }
}

async fn run_chat_stream(
    state: web::Data<AppState>,
    params: InferenceParams,
    id: String,
    model_name: String,
    created: u64,
    has_tools: bool,
) -> HttpResponse {
    let (tx, rx) = mpsc::channel::<web::Bytes>(32);
    let id2 = id.clone();
    let model2 = model_name.clone();

    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        const OBJ: &str = "chat.completion.chunk";

        // First chunk: role delta.
        let _ = tx.blocking_send(sse_chunk(&json!({
            "id": id2, "object": OBJ, "created": created, "model": model2,
            "choices": [{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]
        })));

        // Collect the whole output when tools are present so we can parse tool
        // calls before streaming; otherwise stream token-by-token.
        let mut finish_reason = dllm_inference::FinishReason::Stop;

        if has_tools {
            // Buffered mode: collect, parse, then emit.
            let mut raw = String::new();
            if let Ok((_, fr)) = state2.engine.generate(&params, |piece| {
                raw.push_str(piece);
                true
            }) {
                finish_reason = fr;
            }

            let (content, tool_calls) = extract_tool_calls(&raw);

            if tool_calls.is_empty() {
                // No tool calls — stream content as a single delta.
                let _ = tx.blocking_send(sse_chunk(&json!({
                    "id": id2, "object": OBJ, "created": created, "model": model2,
                    "choices": [{"index":0,"delta":{"content":content},"finish_reason":null}]
                })));
                let _ = tx.blocking_send(sse_chunk(&json!({
                    "id": id2, "object": OBJ, "created": created, "model": model2,
                    "choices": [{"index":0,"delta":{},"finish_reason":finish_reason.as_str()}]
                })));
            } else {
                // Emit tool_calls delta.
                let calls_json: Vec<Value> =
                    tool_calls.iter().map(tools::ToolCall::to_value).collect();
                let content_val = if content.is_empty() {
                    Value::Null
                } else {
                    Value::String(content)
                };
                let _ = tx.blocking_send(sse_chunk(&json!({
                    "id": id2, "object": OBJ, "created": created, "model": model2,
                    "choices": [{"index":0,"delta":{"content":content_val,"tool_calls":calls_json},"finish_reason":null}]
                })));
                let _ = tx.blocking_send(sse_chunk(&json!({
                    "id": id2, "object": OBJ, "created": created, "model": model2,
                    "choices": [{"index":0,"delta":{},"finish_reason":"tool_calls"}]
                })));
            }
        } else {
            // Pure streaming: emit each token piece immediately.
            if let Ok((_, fr)) = state2.engine.generate(&params, |piece| {
                tx.blocking_send(sse_chunk(&json!({
                    "id": id2, "object": OBJ, "created": created, "model": model2,
                    "choices": [{"index":0,"delta":{"content":piece},"finish_reason":null}]
                })))
                .is_ok()
            }) {
                finish_reason = fr;
            }
            let _ = tx.blocking_send(sse_chunk(&json!({
                "id": id2, "object": OBJ, "created": created, "model": model2,
                "choices": [{"index":0,"delta":{},"finish_reason":finish_reason.as_str()}]
            })));
        }

        let _ = tx.blocking_send(sse_done());
    });

    let body_stream = stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, actix_web::Error>(chunk), rx))
    });

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(body_stream)
}

// ---------------------------------------------------------------------------
// Raw completions  POST /v1/completions
// ---------------------------------------------------------------------------

async fn completions(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Bytes,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let text = match std::str::from_utf8(&body) {
        Ok(s) => s.to_owned(),
        Err(_) => return error_response(bad_request("body must be valid UTF-8")),
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return error_response(bad_request(format!("invalid JSON: {e}"))),
    };

    let prompt = match parsed.get("prompt") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            // Array of strings → join (batch not yet supported, take first)
            match arr.first() {
                Some(Value::String(s)) => s.clone(),
                _ => return error_response(bad_request("'prompt' array must contain strings")),
            }
        }
        _ => return error_response(bad_request("'prompt' must be a string")),
    };

    let streaming = parsed
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let params = match InferenceParams::from_request(&parsed, prompt) {
        Ok(p) => p,
        Err(e) => return error_response(http_from(e)),
    };

    let model_name = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(state.engine.model_name())
        .to_owned();
    let created = now_secs();
    let id = format!("cmpl-{created}");

    if streaming {
        run_completion_stream(state, params, id, model_name, created).await
    } else {
        run_completion_blocking(state, params, id, model_name, created).await
    }
}

async fn run_completion_blocking(
    state: web::Data<AppState>,
    params: InferenceParams,
    id: String,
    model_name: String,
    created: u64,
) -> HttpResponse {
    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut text = String::new();
        state2
            .engine
            .generate(&params, |piece| {
                text.push_str(piece);
                true
            })
            .map(|(tokens, reason)| (text, tokens, reason))
    })
    .await;

    match result {
        Ok(Ok((text, completion_tokens, finish_reason))) => {
            HttpResponse::Ok().content_type("application/json").body(
                json!({
                    "id": id,
                    "object": "text_completion",
                    "created": created,
                    "model": model_name,
                    "choices": [{
                        "index": 0,
                        "text": text,
                        "finish_reason": finish_reason.as_str()
                    }],
                    "usage": {
                        "completion_tokens": completion_tokens
                    }
                })
                .to_string(),
            )
        }
        Ok(Err(e)) => error_response(http_from(e)),
        Err(e) => error_response(internal_error(format!("inference task panicked: {e}"))),
    }
}

async fn run_completion_stream(
    state: web::Data<AppState>,
    params: InferenceParams,
    id: String,
    model_name: String,
    created: u64,
) -> HttpResponse {
    let (tx, rx) = mpsc::channel::<web::Bytes>(32);
    let id2 = id.clone();
    let model2 = model_name.clone();

    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut finish_reason = dllm_inference::FinishReason::Stop;
        let result = state2.engine.generate(&params, |piece| {
            let chunk = sse_chunk(&json!({
                "id": id2,
                "object": "text_completion",
                "created": created,
                "model": model2,
                "choices": [{"index": 0, "text": piece, "finish_reason": null}]
            }));
            tx.blocking_send(chunk).is_ok()
        });
        if let Ok((_, fr)) = result {
            finish_reason = fr;
        }
        let last = sse_chunk(&json!({
            "id": id2,
            "object": "text_completion",
            "created": created,
            "model": model2,
            "choices": [{"index": 0, "text": "", "finish_reason": finish_reason.as_str()}]
        }));
        let _ = tx.blocking_send(last);
        let _ = tx.blocking_send(sse_done());
    });

    let body_stream = stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, actix_web::Error>(chunk), rx))
    });

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(body_stream)
}

// ---------------------------------------------------------------------------
// Embeddings  POST /v1/embeddings
// ---------------------------------------------------------------------------

async fn embeddings(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Bytes,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let text = match std::str::from_utf8(&body) {
        Ok(s) => s.to_owned(),
        Err(_) => return error_response(bad_request("body must be valid UTF-8")),
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return error_response(bad_request(format!("invalid JSON: {e}"))),
    };

    // `input` may be a string or an array of strings.
    let inputs: Vec<String> = match parsed.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                match v {
                    Value::String(s) => out.push(s.clone()),
                    _ => return error_response(bad_request("'input' array must contain strings")),
                }
            }
            out
        }
        _ => return error_response(bad_request("'input' must be a string or array of strings")),
    };

    let model_name = parsed
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(state.engine.model_name())
        .to_owned();

    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // Return (vectors, total_prompt_tokens) together so `inputs` doesn't
        // need to be borrowed after the move.
        let total_tokens: u32 = inputs
            .iter()
            .filter_map(|s| state2.engine.token_count(s).ok())
            .sum();
        state2
            .engine
            .embed(&inputs)
            .map(|vecs| (vecs, total_tokens))
            .map_err(http_from)
    })
    .await;

    match result {
        Ok(Ok((vectors, total_tokens))) => {
            let data: Vec<Value> = vectors
                .into_iter()
                .enumerate()
                .map(|(i, v)| {
                    json!({
                        "object": "embedding",
                        "index": i,
                        "embedding": v
                    })
                })
                .collect();
            HttpResponse::Ok().content_type("application/json").body(
                json!({
                    "object": "list",
                    "model": model_name,
                    "data": data,
                    "usage": { "prompt_tokens": total_tokens, "total_tokens": total_tokens }
                })
                .to_string(),
            )
        }
        Ok(Err(e)) => error_response(e),
        Err(e) => error_response(internal_error(format!("embed task panicked: {e}"))),
    }
}

// ---------------------------------------------------------------------------
// File store handlers  POST/GET/DELETE /v1/files
// ---------------------------------------------------------------------------

/// `POST /v1/files`  — upload a file (multipart/form-data with `file` + `purpose`).
async fn upload_file(
    req: HttpRequest,
    state: web::Data<AppState>,
    mut payload: Multipart,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }

    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = "upload".to_owned();
    let mut purpose = "assistants".to_owned();

    while let Some(item) = payload.next().await {
        let mut field = match item {
            Ok(f) => f,
            Err(e) => return error_response(bad_request(format!("multipart error: {e}"))),
        };

        // Read metadata (returns a borrow; we convert to owned before streaming).
        let field_name = field
            .content_disposition()
            .and_then(|cd| cd.get_name())
            .unwrap_or("")
            .to_owned();
        let field_filename = field
            .content_disposition()
            .and_then(|cd| cd.get_filename())
            .map(str::to_owned);

        let mut data: Vec<u8> = Vec::new();
        while let Some(chunk) = field.next().await {
            match chunk {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(e) => return error_response(internal_error(format!("chunk read error: {e}"))),
            }
        }

        match field_name.as_str() {
            "file" => {
                filename = field_filename.unwrap_or_else(|| "upload".to_owned());
                file_bytes = Some(data);
            }
            "purpose" => {
                purpose = String::from_utf8_lossy(&data).into_owned();
            }
            _ => {}
        }
    }

    let Some(bytes) = file_bytes else {
        return error_response(bad_request(
            "'file' field is required (multipart/form-data)",
        ));
    };

    let id = gen_file_id(&bytes);
    let size = bytes.len();
    let created_at = now_secs();

    state.file_store.write().await.insert(
        id.clone(),
        FileEntry {
            id: id.clone(),
            filename: filename.clone(),
            bytes,
            purpose: purpose.clone(),
            created_at,
        },
    );

    tracing::info!("Stored file {id} ({size} bytes, purpose={purpose})");

    HttpResponse::Ok().content_type("application/json").body(
        json!({
            "id": id,
            "object": "file",
            "bytes": size,
            "created_at": created_at,
            "filename": filename,
            "purpose": purpose,
            "status": "processed",
            "status_details": null
        })
        .to_string(),
    )
}

/// `GET /v1/files` — list all uploaded files.
async fn list_files(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let store = state.file_store.read().await;
    let data: Vec<Value> = store
        .values()
        .map(|e| {
            json!({
                "id": e.id,
                "object": "file",
                "bytes": e.bytes.len(),
                "created_at": e.created_at,
                "filename": e.filename,
                "purpose": e.purpose,
            })
        })
        .collect();

    HttpResponse::Ok()
        .content_type("application/json")
        .body(json!({"object": "list", "data": data}).to_string())
}

/// `GET /v1/files/{file_id}` — retrieve file metadata.
async fn get_file(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let id = path.into_inner();
    let store = state.file_store.read().await;
    match store.get(&id) {
        Some(e) => HttpResponse::Ok().content_type("application/json").body(
            json!({
                "id": e.id,
                "object": "file",
                "bytes": e.bytes.len(),
                "created_at": e.created_at,
                "filename": e.filename,
                "purpose": e.purpose,
            })
            .to_string(),
        ),
        None => error_response(HttpError {
            status: StatusCode::NOT_FOUND,
            r#type: "invalid_request_error",
            message: format!("No file with id '{id}'"),
        }),
    }
}

/// `GET /v1/files/{file_id}/content` — download raw file bytes.
async fn get_file_content(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let id = path.into_inner();
    let store = state.file_store.read().await;
    match store.get(&id) {
        Some(e) => HttpResponse::Ok()
            .content_type("application/octet-stream")
            .body(e.bytes.clone()),
        None => error_response(HttpError {
            status: StatusCode::NOT_FOUND,
            r#type: "invalid_request_error",
            message: format!("No file with id '{id}'"),
        }),
    }
}

/// `DELETE /v1/files/{file_id}` — delete an uploaded file.
async fn delete_file(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let id = path.into_inner();
    let removed = state.file_store.write().await.remove(&id).is_some();
    if removed {
        HttpResponse::Ok()
            .content_type("application/json")
            .body(json!({"id": id, "object": "file", "deleted": true}).to_string())
    } else {
        error_response(HttpError {
            status: StatusCode::NOT_FOUND,
            r#type: "invalid_request_error",
            message: format!("No file with id '{id}'"),
        })
    }
}

// ---------------------------------------------------------------------------
// Simple handlers
// ---------------------------------------------------------------------------

async fn list_models(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    HttpResponse::Ok().content_type("application/json").body(
        json!({
            "object": "list",
            "data": [{
                "id": state.engine.model_name(),
                "object": "model",
                "created": now_secs(),
                "owned_by": "llama.cpp",
                "context_length": state.engine.reported_ctx(),
                "embedding_length": state.engine.n_embd()
            }]
        })
        .to_string(),
    )
}

async fn health() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .body(r#"{"status":"ok"}"#)
}

// ---------------------------------------------------------------------------
// Tokenize / detokenize  (llama.cpp server-compatible)
// ---------------------------------------------------------------------------

/// `POST /tokenize` — tokenize a string.
///
/// Body: `{ "content": "...", "add_special": false, "with_pieces": false }`
async fn tokenize_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<Value>,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let content = match body.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => {
            return HttpResponse::Ok()
                .content_type("application/json")
                .body(json!({"tokens": []}).to_string());
        }
        _ => return error_response(bad_request("'content' must be a string")),
    };
    let add_special = body
        .get("add_special")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let with_pieces = body
        .get("with_pieces")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let pieces = state2
            .engine
            .tokenize(&content, add_special, with_pieces)
            .map_err(http_from)?;
        let tokens_json: Value = if with_pieces {
            Value::Array(
                pieces
                    .into_iter()
                    .map(|tp| json!({"id": tp.id, "piece": tp.piece.unwrap_or_default()}))
                    .collect(),
            )
        } else {
            Value::Array(pieces.into_iter().map(|tp| json!(tp.id)).collect())
        };
        Ok::<Value, HttpError>(tokens_json)
    })
    .await;

    let tokens_json = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return error_response(e),
        Err(e) => return error_response(internal_error(format!("task join: {e}"))),
    };

    HttpResponse::Ok()
        .content_type("application/json")
        .body(json!({"tokens": tokens_json}).to_string())
}

/// `POST /detokenize` — detokenize token ids.
///
/// Body: `{ "tokens": [1, 2, 3] }`
async fn detokenize_handler(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<Value>,
) -> HttpResponse {
    if let Some(err) = check_auth(&req, &state) {
        return error_response(err);
    }
    let arr = match body.get("tokens") {
        Some(Value::Array(a)) => a,
        Some(Value::Null) | None => {
            return HttpResponse::Ok()
                .content_type("application/json")
                .body(json!({"content": ""}).to_string());
        }
        _ => return error_response(bad_request("'tokens' must be an array of integers")),
    };
    let mut token_ids: Vec<i32> = Vec::with_capacity(arr.len());
    for v in arr {
        let Some(raw) = v.as_u64() else {
            return error_response(bad_request("each token must be a non-negative integer"));
        };
        let Ok(id) = i32::try_from(raw) else {
            return error_response(bad_request("token id does not fit in i32"));
        };
        token_ids.push(id);
    }

    let permit = state.inference_semaphore.clone().acquire_owned().await;
    let state2 = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        state2.engine.detokenize(&token_ids).map_err(http_from)
    })
    .await;

    let content = match result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return error_response(e),
        Err(e) => return error_response(internal_error(format!("task join: {e}"))),
    };

    HttpResponse::Ok()
        .content_type("application/json")
        .body(json!({"content": content}).to_string())
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // Capture the HF repo ID before `args.model` is consumed. Used later to
    // auto-download the matching mmproj from the same repo.
    #[cfg(feature = "mtmd")]
    let hf_repo: Option<String> = match &args.model {
        ModelArg::HuggingFace { repo, .. } => Some(repo.clone()),
        ModelArg::Local { .. } => None,
    };

    let model_path = args
        .model
        .into_source()
        .resolve()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

    // --print-path: output the resolved path and exit immediately.
    if args.print_path {
        println!("{}", model_path.display());
        return Ok(());
    }

    if args.fit {
        let report = dllm_inference::fit_model(&dllm_inference::FitConfig {
            model_path,
            n_ctx_min: args.fit_n_ctx_min,
            margin_bytes: args.fit_margin_bytes,
            backend_label: active_backend().to_string(),
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }

    let parallel = args.parallel.max(1);
    if args.api_key.is_some() {
        tracing::info!("API key authentication enabled");
    }

    // ── Multimodal projector (optional) ───────────────────────────────────────
    // Resolve the mmproj path:
    //  1. --mmproj given as an absolute/relative path → use as-is.
    //  2. --mmproj given as a bare filename → look next to the model file.
    //  3. --mmproj not given → scan the model's directory, then fall back to
    //     downloading from the same Hugging Face repo.
    #[cfg(feature = "mtmd")]
    let mmproj: Option<dllm_inference::MmprojConfig> = {
        tracing::info!("Model resolved to: {}", model_path.display());
        let model_dir = model_path.parent().unwrap_or(std::path::Path::new("."));
        let mmproj_path: Option<PathBuf> = match &args.mmproj {
            Some(p)
                if p.components().count() == 1 && p.parent() == Some(std::path::Path::new("")) =>
            {
                let candidate = model_path
                    .parent()
                    .map(|d| d.join(p))
                    .filter(|f| f.exists());
                if candidate.is_none() {
                    tracing::warn!(
                        "mmproj '{}' not found next to model ({}); skipping multimodal",
                        p.display(),
                        model_dir.display()
                    );
                }
                candidate
            }
            Some(p) => Some(p.clone()),
            None => dllm_inference::find_mmproj_in_dir(model_dir).or_else(|| {
                hf_repo
                    .as_deref()
                    .and_then(dllm_inference::download_mmproj_from_hf)
            }),
        };
        mmproj_path.map(|path| dllm_inference::MmprojConfig {
            path,
            use_gpu: !args.no_mmproj_gpu,
            n_threads: args.mmproj_n_threads,
        })
    };

    #[cfg(not(feature = "mtmd"))]
    if args.mmproj.is_some() {
        tracing::warn!(
            "--mmproj was provided but this binary was compiled without the `mtmd` feature. \
             Rebuild with `--features mtmd` to enable multimodal support."
        );
    }

    let config = ModelConfig {
        model_path,
        n_gpu_layers: args.n_gpu_layers,
        ctx_size: args.ctx_size,
        #[cfg(feature = "mtmd")]
        mmproj,
    };
    let engine = InferenceModel::load(config).map_err(|e| std::io::Error::other(e.to_string()))?;

    let state = web::Data::new(AppState {
        engine,
        inference_semaphore: Arc::new(Semaphore::new(parallel)),
        api_key: args.api_key,
        file_store: Arc::new(RwLock::new(HashMap::new())),
    });

    let addr = format!("{}:{}", args.host, args.port);
    tracing::info!("Listening on http://{addr}  (parallel={parallel})");
    tracing::info!("Endpoints:");
    tracing::info!("  GET    /health  /v1/health");
    tracing::info!("  GET    /v1/models");
    tracing::info!("  POST   /v1/chat/completions  /chat/completions  (streaming)");
    tracing::info!("  POST   /v1/completions       /completions       (streaming)");
    tracing::info!("  POST   /v1/embeddings        /embeddings");
    tracing::info!("  POST   /tokenize  /detokenize");
    tracing::info!("  POST   /v1/files             (upload image/audio for multimodal)");
    tracing::info!("  GET    /v1/files             (list uploaded files)");
    tracing::info!("  GET    /v1/files/{{id}}        (file metadata)");
    tracing::info!("  GET    /v1/files/{{id}}/content (download file)");
    tracing::info!("  DELETE /v1/files/{{id}}        (delete file)");

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(web::JsonConfig::default().error_handler(|err, _req| {
                let msg = format!("JSON parse error: {err}");
                actix_web::error::InternalError::from_response(
                    err,
                    error_response(bad_request(msg)),
                )
                .into()
            }))
            .route("/health", web::get().to(health))
            .route("/v1/health", web::get().to(health))
            .route("/v1/models", web::get().to(list_models))
            .route("/v1/chat/completions", web::post().to(chat_completions))
            .route("/chat/completions", web::post().to(chat_completions))
            .route("/v1/completions", web::post().to(completions))
            .route("/completions", web::post().to(completions))
            .route("/v1/embeddings", web::post().to(embeddings))
            .route("/embeddings", web::post().to(embeddings))
            .route("/tokenize", web::post().to(tokenize_handler))
            .route("/detokenize", web::post().to(detokenize_handler))
            // File store
            .route("/v1/files", web::post().to(upload_file))
            .route("/v1/files", web::get().to(list_files))
            .route("/v1/files/{file_id}", web::get().to(get_file))
            .route(
                "/v1/files/{file_id}/content",
                web::get().to(get_file_content),
            )
            .route("/v1/files/{file_id}", web::delete().to(delete_file))
    })
    .bind(&addr)?
    .run()
    .await
}
