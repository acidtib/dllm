//! Sampling parameters and the text/multimodal generation loops.

use crate::{InferenceError, InferenceModel};
use llama_cpp_4::prelude::*;
use serde_json::Value;
use std::num::NonZeroU32;
// Override the llama.cpp prelude's `Result<T>` alias with the std two-parameter
// `Result` used throughout this module.
use std::result::Result;

/// All sampling / generation parameters for one request.
#[derive(Debug, Clone)]
pub struct InferenceParams {
    pub prompt: String,
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: i32,
    pub seed: u32,
    pub max_tokens: u32,
    pub stop_seqs: Vec<String>,
    /// Optional GBNF grammar string.
    pub grammar: Option<String>,
    /// Raw bytes for each media item (image or audio), in marker order. Only
    /// used by the multimodal path; the caller resolves sources to bytes.
    pub image_bytes: Vec<Vec<u8>>,
}

impl InferenceParams {
    /// Extracts sampling parameters from an OpenAI-style request body, pairing
    /// them with an already-rendered `prompt`.
    pub fn from_request(req: &Value, prompt: String) -> Result<Self, InferenceError> {
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
        let seed = req.get("seed").and_then(Value::as_u64).unwrap_or(0) as u32;
        let max_tokens = parse_max_tokens(req)?;
        let grammar = match req.get("grammar") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            _ => return Err(InferenceError::invalid("'grammar' must be a GBNF string")),
        };
        let stop_seqs = parse_stop_sequences(req)?;
        Ok(InferenceParams {
            prompt,
            temperature,
            top_p,
            top_k,
            seed,
            max_tokens,
            stop_seqs,
            grammar,
            image_bytes: Vec::new(),
        })
    }
}

/// `OpenAI` uses `max_tokens`; newer clients may send `max_completion_tokens`.
fn parse_max_tokens(req: &Value) -> Result<u32, InferenceError> {
    let raw = req
        .get("max_completion_tokens")
        .or_else(|| req.get("max_tokens"));
    match raw {
        None | Some(Value::Null) => Ok(1024),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                InferenceError::invalid("'max_tokens' must be a positive integer")
            })?;
            if n == 0 {
                return Err(InferenceError::invalid("'max_tokens' must be > 0"));
            }
            u32::try_from(n).map_err(|_| InferenceError::invalid("'max_tokens' is too large"))
        }
    }
}

fn parse_stop_sequences(req: &Value) -> Result<Vec<String>, InferenceError> {
    match req.get("stop") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                _ => Err(InferenceError::invalid(
                    "each element of 'stop' must be a string",
                )),
            })
            .collect(),
        _ => Err(InferenceError::invalid(
            "'stop' must be a string or array of strings",
        )),
    }
}

/// Why the decode loop stopped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
}

impl FinishReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
        }
    }
}

/// Minimum context size needed to hold the prompt + generated tokens. Only the
/// multimodal path sizes context this way.
#[cfg(feature = "mtmd")]
fn n_ctx_for_params(params: &InferenceParams) -> u32 {
    // Rough upper bound: 4 chars per token on average.
    let prompt_est = (params.prompt.len() / 4 + 1) as u32;
    prompt_est + params.max_tokens
}

impl InferenceModel {
    /// Runs generation, calling `on_piece` for each decoded text fragment.
    /// `on_piece` returns `false` to stop early (e.g. a cancelled stream).
    /// Returns `(completion_token_count, finish_reason)`.
    pub fn generate<F>(
        &self,
        params: &InferenceParams,
        on_piece: F,
    ) -> Result<(u32, FinishReason), InferenceError>
    where
        F: FnMut(&str) -> bool,
    {
        #[cfg(feature = "mtmd")]
        if !params.image_bytes.is_empty() {
            return if self.mtmd_ctx.is_some() {
                self.generate_multimodal(params, on_piece)
            } else {
                tracing::warn!(
                    "Request contains {} image(s) but the model was loaded without a projector; \
                     images will be ignored and the prompt will be processed as text.",
                    params.image_bytes.len()
                );
                self.generate_text(params, on_piece)
            };
        }

        self.generate_text(params, on_piece)
    }

    fn generate_text<F>(
        &self,
        params: &InferenceParams,
        mut on_piece: F,
    ) -> Result<(u32, FinishReason), InferenceError>
    where
        F: FnMut(&str) -> bool,
    {
        let tokens = self
            .model
            .str_to_token(&params.prompt, AddBos::Always)
            .map_err(|e| InferenceError::internal(format!("tokenisation failed: {e}")))?;

        let n_prompt = tokens.len() as u32;

        // When no explicit ctx-size is set, default to the model's training
        // context but cap it at 4096. n_ctx_train for modern models can be
        // 32 K–128 K tokens; allocating a full-size KV cache + compute buffer
        // for every request consumes tens of GB and reliably triggers OOM.
        const DEFAULT_MAX_CTX: u32 = 4096;
        let n_ctx = self
            .default_ctx_size
            .map_or_else(
                || self.model.n_ctx_train().min(DEFAULT_MAX_CTX),
                NonZeroU32::get,
            )
            .max(n_prompt + params.max_tokens);

        // n_batch controls the compute-buffer size inside llama.cpp. Matching
        // it to n_ctx when n_ctx is large (e.g. 32 K) allocates a huge scratch
        // buffer even if the sequence is short. Cap it independently.
        const DEFAULT_MAX_BATCH: u32 = 2048;
        let n_batch = n_ctx.min(DEFAULT_MAX_BATCH);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_batch(n_batch);

        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| InferenceError::internal(format!("context init: {e}")))?;

        let mut batch = LlamaBatch::new(n_ctx as usize, 1);
        let last = tokens.len().saturating_sub(1) as i32;
        for (i, &tok) in tokens.iter().enumerate() {
            batch
                .add(tok, i as i32, &[0], i as i32 == last)
                .map_err(|e| InferenceError::internal(format!("batch add: {e}")))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| InferenceError::internal(format!("prefill: {e}")))?;

        let mut chain: Vec<LlamaSampler> = Vec::new();
        if let Some(gbnf) = &params.grammar {
            chain.push(LlamaSampler::grammar(&self.model, gbnf, "root"));
        }
        if params.temperature > 0.0 {
            if params.top_k > 0 {
                chain.push(LlamaSampler::top_k(params.top_k));
            }
            if params.top_p < 1.0 {
                chain.push(LlamaSampler::top_p(params.top_p, 1));
            }
            chain.push(LlamaSampler::temp(params.temperature));
            chain.push(LlamaSampler::dist(params.seed));
        } else {
            chain.push(LlamaSampler::greedy());
        }
        let sampler = LlamaSampler::chain_simple(chain);

        let mut n_cur = batch.n_tokens();
        let max_pos = n_cur + params.max_tokens as i32;
        let mut completion_tokens: u32 = 0;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut finish_reason = FinishReason::Stop;

        // For stop-sequence detection we keep a small rolling window of recently
        // generated (but not-yet-emitted) text. Everything before the window is
        // already forwarded to `on_piece`, so we never re-emit it when a stop
        // sequence is finally matched.
        let max_stop_len = params
            .stop_seqs
            .iter()
            .map(std::string::String::len)
            .max()
            .unwrap_or(0);
        let mut window = String::new();
        let mut cancelled = false;

        'decode: loop {
            if n_cur >= max_pos {
                finish_reason = FinishReason::Length;
                break;
            }

            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            if self.model.is_eog_token(token) {
                break;
            }

            let bytes = self
                .model
                .token_to_bytes(token, Special::Plaintext)
                .map_err(|e| InferenceError::internal(format!("token_to_bytes: {e}")))?;
            let mut piece = String::with_capacity(8);
            let _ = decoder.decode_to_string(&bytes, &mut piece, false);
            completion_tokens += 1;

            window.push_str(&piece);

            for stop in &params.stop_seqs {
                if !stop.is_empty() && window.ends_with(stop.as_str()) {
                    let emit_len = window.len() - stop.len();
                    if emit_len > 0 {
                        let _ = on_piece(&window[..emit_len]);
                    }
                    break 'decode;
                }
            }

            if max_stop_len == 0 {
                if !on_piece(&window) {
                    cancelled = true;
                    break;
                }
                window.clear();
            } else {
                let keep = window.len().min(max_stop_len);
                let emit_len = window.len().saturating_sub(keep);
                if emit_len > 0 {
                    if !on_piece(&window[..emit_len]) {
                        cancelled = true;
                        break;
                    }
                    let remaining = window[emit_len..].to_owned();
                    window = remaining;
                }
            }

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| InferenceError::internal(format!("batch add: {e}")))?;
            n_cur += 1;
            ctx.decode(&mut batch)
                .map_err(|e| InferenceError::internal(format!("decode: {e}")))?;
        }

        if !cancelled && !window.is_empty() {
            let _ = on_piece(&window);
        }

        Ok((completion_tokens, finish_reason))
    }

    /// Multimodal generation: encode images with mtmd, then decode as normal.
    #[cfg(feature = "mtmd")]
    fn generate_multimodal<F>(
        &self,
        params: &InferenceParams,
        mut on_piece: F,
    ) -> Result<(u32, FinishReason), InferenceError>
    where
        F: FnMut(&str) -> bool,
    {
        let mtmd_ctx = self
            .mtmd_ctx
            .as_ref()
            .expect("generate_multimodal called without mtmd_ctx");

        // Vision models often embed 256–1024 tokens per image, so default to 8 K.
        const MM_DEFAULT_CTX: u32 = 8192;
        let n_ctx = self
            .default_ctx_size
            .map_or_else(
                || self.model.n_ctx_train().min(MM_DEFAULT_CTX),
                NonZeroU32::get,
            )
            .max(n_ctx_for_params(params));

        let n_batch = n_ctx.min(2048);

        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(n_ctx))
            .with_n_batch(n_batch);

        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| InferenceError::internal(format!("context init: {e}")))?;

        let bitmaps: Vec<MtmdBitmap> = params
            .image_bytes
            .iter()
            .enumerate()
            .map(|(i, bytes)| {
                MtmdBitmap::from_buf(mtmd_ctx, bytes)
                    .map_err(|e| InferenceError::internal(format!("bitmap {i}: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let input_text = MtmdInputText::new(&params.prompt, true, true);
        let bitmap_refs: Vec<&MtmdBitmap> = bitmaps.iter().collect();
        let mut chunks = MtmdInputChunks::new();
        mtmd_ctx
            .tokenize(&input_text, &bitmap_refs, &mut chunks)
            .map_err(|e| InferenceError::internal(format!("mtmd_tokenize: {e}")))?;

        tracing::info!(
            "generate_multimodal: {} image(s), prompt_len={}",
            params.image_bytes.len(),
            params.prompt.len()
        );

        let mut n_past: i32 = 0;
        tracing::info!("Calling eval_chunks…");
        mtmd_ctx
            .eval_chunks(
                ctx.as_ptr(),
                &chunks,
                /* n_past */ 0,
                /* seq_id */ 0,
                n_batch as i32,
                /* logits_last */ true,
                &mut n_past,
            )
            .map_err(|e| InferenceError::internal(format!("mtmd_eval_chunks: {e}")))?;
        tracing::info!("eval_chunks done, n_past={n_past}");

        let mut chain: Vec<LlamaSampler> = Vec::new();
        if let Some(gbnf) = &params.grammar {
            chain.push(LlamaSampler::grammar(&self.model, gbnf, "root"));
        }
        if params.temperature > 0.0 {
            if params.top_k > 0 {
                chain.push(LlamaSampler::top_k(params.top_k));
            }
            if params.top_p < 1.0 {
                chain.push(LlamaSampler::top_p(params.top_p, 1));
            }
            chain.push(LlamaSampler::temp(params.temperature));
            chain.push(LlamaSampler::dist(params.seed));
        } else {
            chain.push(LlamaSampler::greedy());
        }
        let sampler = LlamaSampler::chain_simple(chain);

        let max_pos = n_past + params.max_tokens as i32;
        let mut completion_tokens: u32 = 0;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut finish_reason = FinishReason::Stop;

        let max_stop_len = params.stop_seqs.iter().map(|s| s.len()).max().unwrap_or(0);
        let mut window = String::new();
        let mut cancelled = false;

        let mut batch = LlamaBatch::new(1, 0);

        'decode: loop {
            if n_past >= max_pos {
                finish_reason = FinishReason::Length;
                break;
            }

            // -1 means "sample from the last position with logits computed".
            // After eval_chunks this is always correct.
            let token = sampler.sample(&ctx, -1);
            if self.model.is_eog_token(token) {
                break;
            }

            let bytes = self
                .model
                .token_to_bytes(token, Special::Plaintext)
                .map_err(|e| InferenceError::internal(format!("token_to_bytes: {e}")))?;
            let mut piece = String::with_capacity(8);
            let _ = decoder.decode_to_string(&bytes, &mut piece, false);
            completion_tokens += 1;

            window.push_str(&piece);

            for stop in &params.stop_seqs {
                if !stop.is_empty() && window.ends_with(stop.as_str()) {
                    let emit_len = window.len() - stop.len();
                    if emit_len > 0 {
                        let _ = on_piece(&window[..emit_len]);
                    }
                    break 'decode;
                }
            }

            if max_stop_len == 0 {
                if !on_piece(&window) {
                    cancelled = true;
                    break;
                }
                window.clear();
            } else {
                let keep = window.len().min(max_stop_len);
                let emit_len = window.len().saturating_sub(keep);
                if emit_len > 0 {
                    if !on_piece(&window[..emit_len]) {
                        cancelled = true;
                        break;
                    }
                    let remaining = window[emit_len..].to_owned();
                    window = remaining;
                }
            }

            batch.clear();
            batch
                .add(token, n_past, &[0], true)
                .map_err(|e| InferenceError::internal(format!("batch add: {e}")))?;
            n_past += 1;
            ctx.decode(&mut batch)
                .map_err(|e| InferenceError::internal(format!("decode: {e}")))?;
        }

        if !cancelled && !window.is_empty() {
            let _ = on_piece(&window);
        }

        Ok((completion_tokens, finish_reason))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_apply_when_fields_absent() {
        let params = InferenceParams::from_request(&json!({}), "hi".to_string()).unwrap();
        assert_eq!(params.max_tokens, 1024);
        assert_eq!(params.temperature, 1.0);
        assert_eq!(params.top_p, 1.0);
        assert_eq!(params.top_k, 0);
        assert!(params.stop_seqs.is_empty());
        assert!(params.grammar.is_none());
    }

    #[test]
    fn negative_temperature_is_invalid() {
        let err = InferenceParams::from_request(&json!({ "temperature": -0.1 }), String::new())
            .unwrap_err();
        assert!(err.is_invalid());
    }

    #[test]
    fn zero_max_tokens_is_invalid() {
        let err =
            InferenceParams::from_request(&json!({ "max_tokens": 0 }), String::new()).unwrap_err();
        assert!(err.is_invalid());
    }

    #[test]
    fn max_completion_tokens_takes_precedence() {
        let params = InferenceParams::from_request(
            &json!({ "max_completion_tokens": 7, "max_tokens": 99 }),
            String::new(),
        )
        .unwrap();
        assert_eq!(params.max_tokens, 7);
    }

    #[test]
    fn stop_accepts_string_or_array() {
        let one = InferenceParams::from_request(&json!({ "stop": "END" }), String::new()).unwrap();
        assert_eq!(one.stop_seqs, vec!["END".to_string()]);
        let many =
            InferenceParams::from_request(&json!({ "stop": ["A", "B"] }), String::new()).unwrap();
        assert_eq!(many.stop_seqs, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn finish_reason_strings() {
        assert_eq!(FinishReason::Stop.as_str(), "stop");
        assert_eq!(FinishReason::Length.as_str(), "length");
    }
}
