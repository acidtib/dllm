//! Framework-agnostic OpenAI chat-completions orchestration.
//!
//! This is the shared chat protocol layer used by the embedded runtime in
//! `dllmd`. It has no web-framework or transport dependency: the caller reads
//! the request body, resolves any image sources, runs generation (blocking),
//! and frames the emitted chunks (SSE) itself.
//!
//! Flow:
//! 1. [`prepare_chat`] validates parameters (before the expensive prompt
//!    render), normalizes messages, injects tools, renders the prompt, and
//!    builds [`PreparedChat`] (sync, no network).
//! 2. The caller resolves [`PreparedChat::image_sources`] to bytes and assigns
//!    them to `prepared.params.image_bytes` (transport-specific: file store,
//!    URL fetch).
//! 3. [`chat_blocking`] or [`chat_stream`] runs generation. These call into
//!    llama.cpp and must run on a blocking thread.

pub mod tools;

use crate::{FinishReason, InferenceError, InferenceModel, InferenceParams};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "mtmd")]
pub use tools::ImageSource;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Response envelope fields shared by the streaming and non-streaming paths.
#[derive(Debug, Clone)]
pub struct ChatMeta {
    pub model_name: String,
    pub id: String,
    pub created: u64,
    pub has_tools: bool,
    pub streaming: bool,
}

/// A validated, prompt-rendered chat request ready for generation.
pub struct PreparedChat {
    pub params: InferenceParams,
    /// Image/audio sources referenced by the request, in marker order. The
    /// caller resolves these to bytes and assigns them to `params.image_bytes`.
    #[cfg(feature = "mtmd")]
    pub image_sources: Vec<ImageSource>,
    pub meta: ChatMeta,
}

/// Validate sampling parameters before the expensive prompt render so invalid
/// requests return a caller error rather than failing later. Mirrors the checks
/// in [`InferenceParams::from_request`] but runs first.
fn validate_sampling(req: &Value) -> Result<(), InferenceError> {
    let temperature = req
        .get("temperature")
        .and_then(Value::as_f64)
        .unwrap_or(1.0) as f32;
    if temperature < 0.0 {
        return Err(InferenceError::invalid("'temperature' must be >= 0"));
    }
    let top_p = req.get("top_p").and_then(Value::as_f64).unwrap_or(1.0) as f32;
    if !(0.0 < top_p && top_p <= 1.0) {
        return Err(InferenceError::invalid("'top_p' must be in (0, 1]"));
    }
    let top_k = req.get("top_k").and_then(Value::as_i64).unwrap_or(0) as i32;
    if top_k < 0 {
        return Err(InferenceError::invalid("'top_k' must be >= 0"));
    }
    let max_raw = req
        .get("max_completion_tokens")
        .or_else(|| req.get("max_tokens"));
    match max_raw {
        None | Some(Value::Null) => {}
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                InferenceError::invalid("'max_tokens' must be a positive integer")
            })?;
            if n == 0 {
                return Err(InferenceError::invalid("'max_tokens' must be > 0"));
            }
            u32::try_from(n).map_err(|_| InferenceError::invalid("'max_tokens' is too large"))?;
        }
    }
    if matches!(req.get("grammar"), Some(v) if !v.is_string() && !v.is_null()) {
        return Err(InferenceError::invalid("'grammar' must be a GBNF string"));
    }
    Ok(())
}

/// Parse and render a chat request into a [`PreparedChat`]. Does not touch the
/// network or run generation.
pub fn prepare_chat(engine: &InferenceModel, req: &Value) -> Result<PreparedChat, InferenceError> {
    validate_sampling(req)?;
    let streaming = req.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let tool_defs = tools::parse_tools(req)?;
    let tool_choice = tools::parse_tool_choice(req)?;

    // Always run the multimodal parser so image parts are counted even without a
    // projector; fall back to the text-only normaliser when images are present
    // but unusable so markers are not left in the prompt.
    #[cfg(feature = "mtmd")]
    let (base_msg_pairs, image_sources) = {
        let marker = crate::mtmd_default_marker();
        let (pairs, sources) = tools::normalise_messages_multimodal(req, &marker)?;
        if !sources.is_empty() && !engine.has_mtmd() {
            tracing::warn!(
                n_images = sources.len(),
                "Request contains image(s) but the model was loaded without a projector. \
                 Images will be IGNORED and the prompt processed as plain text."
            );
            (tools::normalise_messages(req)?, Vec::new())
        } else {
            (pairs, sources)
        }
    };

    #[cfg(not(feature = "mtmd"))]
    let base_msg_pairs = tools::normalise_messages(req)?;

    let prompt = {
        let mut msg_pairs = base_msg_pairs;
        tools::inject_tools(&mut msg_pairs, &tool_defs, &tool_choice);
        let template_override = match req.get("chat_template") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            _ => return Err(InferenceError::invalid("'chat_template' must be a string")),
        };
        engine.render_prompt(template_override.as_deref(), &msg_pairs)?
    };

    let mut params = InferenceParams::from_request(req, prompt)?;

    // Give tool-calling models room to emit their reasoning plus a full tool
    // call. Grammar forcing is intentionally not used; it conflicts with the
    // special tokens capable models use for tool calls.
    if !tool_defs.is_empty() && params.max_tokens < 1024 {
        params.max_tokens = 1024;
    }

    let model_name = req
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(engine.model_name())
        .to_owned();
    let created = now_secs();
    let id = format!("chatcmpl-{created}");

    Ok(PreparedChat {
        params,
        #[cfg(feature = "mtmd")]
        image_sources,
        meta: ChatMeta {
            model_name,
            id,
            created,
            has_tools: !tool_defs.is_empty(),
            streaming,
        },
    })
}

/// Run generation and build the non-streaming `chat.completion` response. Must
/// run on a blocking thread.
pub fn chat_blocking(
    engine: &InferenceModel,
    prepared: &PreparedChat,
) -> Result<Value, InferenceError> {
    let mut raw = String::new();
    let (completion_tokens, finish_reason) = engine.generate(&prepared.params, |piece| {
        raw.push_str(piece);
        true
    })?;

    let prompt_tokens = 0u32; // cheap approximation; full count needs a 2nd tokenise pass
    let (content, tool_calls) = if prepared.meta.has_tools {
        tools::extract_tool_calls(&raw)
    } else {
        (raw, vec![])
    };

    let (final_finish, message) = if tool_calls.is_empty() {
        (
            finish_reason.as_str(),
            json!({ "role": "assistant", "content": content }),
        )
    } else {
        let calls_json: Vec<Value> = tool_calls.iter().map(tools::ToolCall::to_value).collect();
        (
            "tool_calls",
            json!({
                "role": "assistant",
                "content": if content.is_empty() { Value::Null } else { Value::String(content) },
                "tool_calls": calls_json
            }),
        )
    };

    Ok(json!({
        "id": prepared.meta.id,
        "object": "chat.completion",
        "created": prepared.meta.created,
        "model": prepared.meta.model_name,
        "choices": [{"index": 0, "message": message, "finish_reason": final_finish}],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens
        }
    }))
}

/// Run generation and emit `chat.completion.chunk` objects in order (role
/// delta, content deltas, finish delta). The caller frames each as an SSE
/// `data:` line and appends the terminating `[DONE]`. `emit` returns `false`
/// when the client is gone, which stops generation. Must run on a blocking
/// thread. Generation errors terminate the stream cleanly rather than surfacing
/// (the response headers are already sent).
pub fn chat_stream(
    engine: &InferenceModel,
    prepared: &PreparedChat,
    mut emit: impl FnMut(Value) -> bool,
) {
    const OBJ: &str = "chat.completion.chunk";
    let id = prepared.meta.id.clone();
    let model = prepared.meta.model_name.clone();
    let created = prepared.meta.created;

    // First chunk: role delta.
    let _ = emit(json!({
        "id": id, "object": OBJ, "created": created, "model": model,
        "choices": [{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]
    }));

    let mut finish_reason = FinishReason::Stop;

    if prepared.meta.has_tools {
        // Buffered mode: collect, parse tool calls, then emit.
        let mut raw = String::new();
        if let Ok((_, fr)) = engine.generate(&prepared.params, |piece| {
            raw.push_str(piece);
            true
        }) {
            finish_reason = fr;
        }

        let (content, tool_calls) = tools::extract_tool_calls(&raw);
        if tool_calls.is_empty() {
            let _ = emit(json!({
                "id": id, "object": OBJ, "created": created, "model": model,
                "choices": [{"index":0,"delta":{"content":content},"finish_reason":null}]
            }));
            let _ = emit(json!({
                "id": id, "object": OBJ, "created": created, "model": model,
                "choices": [{"index":0,"delta":{},"finish_reason":finish_reason.as_str()}]
            }));
        } else {
            let calls_json: Vec<Value> = tool_calls.iter().map(tools::ToolCall::to_value).collect();
            let content_val = if content.is_empty() {
                Value::Null
            } else {
                Value::String(content)
            };
            let _ = emit(json!({
                "id": id, "object": OBJ, "created": created, "model": model,
                "choices": [{"index":0,"delta":{"content":content_val,"tool_calls":calls_json},"finish_reason":null}]
            }));
            let _ = emit(json!({
                "id": id, "object": OBJ, "created": created, "model": model,
                "choices": [{"index":0,"delta":{},"finish_reason":"tool_calls"}]
            }));
        }
    } else {
        // Pure streaming: emit each token piece immediately.
        if let Ok((_, fr)) = engine.generate(&prepared.params, |piece| {
            emit(json!({
                "id": id, "object": OBJ, "created": created, "model": model,
                "choices": [{"index":0,"delta":{"content":piece},"finish_reason":null}]
            }))
        }) {
            finish_reason = fr;
        }
        let _ = emit(json!({
            "id": id, "object": OBJ, "created": created, "model": model,
            "choices": [{"index":0,"delta":{},"finish_reason":finish_reason.as_str()}]
        }));
    }
}

// ---------------------------------------------------------------------------
// Multimodal image fetching (mtmd only)
// ---------------------------------------------------------------------------

/// Fetch the raw bytes for a multimodal image source: decode a `data:` URI or
/// download an `http(s)://` URL. Shared by every consumer that resolves
/// [`ImageSource::Url`] so the fetch behavior stays identical.
#[cfg(feature = "mtmd")]
pub async fn fetch_image_url(url: &str) -> Result<Vec<u8>, InferenceError> {
    tracing::info!("Fetching image: {}…", &url[..url.len().min(120)]);
    if let Some(rest) = url.strip_prefix("data:") {
        // data:[<mediatype>][;base64],<data>
        let comma = rest
            .find(',')
            .ok_or_else(|| InferenceError::invalid("invalid data URI: missing ','"))?;
        let meta = &rest[..comma];
        let data = &rest[comma + 1..];
        if meta.ends_with(";base64") {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| InferenceError::invalid(format!("base64 decode error: {e}")))?;
            tracing::info!("Decoded {} bytes from data URI", bytes.len());
            Ok(bytes)
        } else {
            // Plain text / URL-encoded — treat the raw bytes as the payload.
            Ok(data.as_bytes().to_vec())
        }
    } else if url.starts_with("http://") || url.starts_with("https://") {
        // Many CDNs block requests that lack a browser-like User-Agent and
        // return an HTML error page instead of the image; stb_image then fails.
        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (compatible; llama-cpp-rs; \
                 +https://github.com/utilityai/llama-cpp-rs)",
            )
            .build()
            .map_err(|e| InferenceError::internal(format!("reqwest client: {e}")))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| InferenceError::invalid(format!("failed to fetch image URL: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(InferenceError::invalid(format!(
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
            .map_err(|e| InferenceError::invalid(format!("failed to read image response: {e}")))?;

        tracing::info!(
            "Downloaded {} bytes (content-type: {content_type}) from URL",
            bytes.len()
        );

        // Anything under 1 KB cannot be a real image — surface the body so the
        // caller can see what the server actually returned.
        if bytes.len() < 1024 {
            let preview = std::str::from_utf8(&bytes).unwrap_or("(binary)");
            return Err(InferenceError::invalid(format!(
                "image URL returned only {} bytes — not a valid image file. \
                 Response body: {preview:?}",
                bytes.len()
            )));
        }

        // Warn if the response looks like HTML rather than binary image data.
        if bytes.starts_with(b"<!") || bytes.starts_with(b"<h") || bytes.starts_with(b"<H") {
            return Err(InferenceError::invalid(format!(
                "image URL returned HTML instead of an image. \
                 The server likely rejected the request (check the URL and any auth). \
                 First 200 bytes: {:?}",
                std::str::from_utf8(&bytes[..bytes.len().min(200)]).unwrap_or("(invalid utf-8)")
            )));
        }

        Ok(bytes.to_vec())
    } else {
        Err(InferenceError::invalid(
            "unsupported image source: must start with 'data:', 'http://', or 'https://'",
        ))
    }
}
