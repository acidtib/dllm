//! In-process inference core for DLLM.
//!
//! This crate owns every llama.cpp call: model resolution and loading, memory
//! fitting, prompt rendering, text and multimodal generation, embeddings, and
//! tokenization. It is transport-agnostic. `dllmd` embeds it directly and
//! serves inference in-process.
//!
//! Errors are reported through [`InferenceError`], which distinguishes a caller
//! mistake ([`InferenceError::InvalidRequest`]) from an internal failure
//! ([`InferenceError::Internal`]) so an HTTP adapter can map them to 400 and 500
//! without inspecting message text.
//!
//! Backend identity is data, not a compile-time constant. Fitting takes a
//! backend label from the caller (discovery in `dllmd`), so this library does
//! not decide which accelerator is "active".

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::case_sensitive_file_extension_comparisons
)]

mod embed;
mod fit;
mod generate;
pub mod openai;
mod resolve;
mod tokenize;

use llama_cpp_4::prelude::*;
use std::sync::{Mutex, OnceLock};
use std::{num::NonZeroU32, path::PathBuf};
// The llama.cpp prelude re-exports its own `Result<T>` alias; keep the std one
// so our `Result<_, InferenceError>` signatures resolve correctly.
use std::result::Result;

/// The llama.cpp backend is a process-global singleton: `LlamaBackend::init`
/// may be called only once per process and errors afterward. Both model loading
/// and fitting run inside the daemon process, so they must share one backend
/// rather than each initializing their own. Returns a `'static` reference to the
/// single backend, initializing it on first use.
pub(crate) fn shared_backend() -> anyhow::Result<&'static LlamaBackend> {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    static INIT_LOCK: Mutex<()> = Mutex::new(());
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    // Serialize the fallible init so `LlamaBackend::init` runs exactly once even
    // under concurrent first callers (OnceLock has no stable fallible init).
    let _guard = INIT_LOCK.lock().unwrap();
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let backend = LlamaBackend::init()?;
    let _ = BACKEND.set(backend);
    Ok(BACKEND.get().expect("backend was just set"))
}

pub use fit::{fit_model, FitConfig, FitReport};
pub use generate::{FinishReason, InferenceParams};
pub use resolve::ModelSource;
#[cfg(feature = "mtmd")]
pub use resolve::{download_mmproj_from_hf, find_mmproj_in_dir};
pub use tokenize::TokenPiece;

/// A caller mistake or an internal failure during inference.
#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    /// The request was malformed or out of range. Maps to HTTP 400.
    #[error("{0}")]
    InvalidRequest(String),
    /// The model or runtime failed. Maps to HTTP 500.
    #[error("{0}")]
    Internal(String),
}

impl InferenceError {
    pub fn invalid(message: impl Into<String>) -> Self {
        InferenceError::InvalidRequest(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        InferenceError::Internal(message.into())
    }

    /// True for a caller mistake (HTTP 400), false for an internal failure
    /// (HTTP 500).
    pub fn is_invalid(&self) -> bool {
        matches!(self, InferenceError::InvalidRequest(_))
    }
}

/// Resolved multimodal projector, ready to load onto a model.
#[cfg(feature = "mtmd")]
#[derive(Debug, Clone)]
pub struct MmprojConfig {
    pub path: PathBuf,
    pub use_gpu: bool,
    pub n_threads: i32,
}

/// Everything needed to load a model into an [`InferenceModel`].
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub model_path: PathBuf,
    pub n_gpu_layers: u32,
    /// Context-size override. `None` keeps the model's trained context length
    /// (capped per request inside generation).
    pub ctx_size: Option<NonZeroU32>,
    #[cfg(feature = "mtmd")]
    pub mmproj: Option<MmprojConfig>,
}

/// A loaded model plus its backend and optional multimodal context. Holds the
/// llama.cpp state for a single served model. llama.cpp contexts are not
/// thread-safe, so callers serialize generation themselves (the server uses a
/// semaphore); a fresh context is created per request.
pub struct InferenceModel {
    pub(crate) backend: &'static LlamaBackend,
    pub(crate) model: LlamaModel,
    pub(crate) chat_template: Option<String>,
    pub(crate) model_name: String,
    pub(crate) default_ctx_size: Option<NonZeroU32>,
    #[cfg(feature = "mtmd")]
    pub(crate) mtmd_ctx: Option<MtmdContext>,
}

impl InferenceModel {
    /// Initializes the backend, loads the model, reads the built-in chat
    /// template, and (when configured) loads the multimodal projector.
    pub fn load(config: ModelConfig) -> anyhow::Result<Self> {
        let model_name = config
            .model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("llama.cpp")
            .to_string();

        let backend = shared_backend()?;

        let mut model_params = LlamaModelParams::default();
        if config.n_gpu_layers > 0 {
            model_params = model_params.with_n_gpu_layers(config.n_gpu_layers);
        }

        let model = LlamaModel::load_from_file(backend, &config.model_path, &model_params)?;

        let chat_template = model.get_chat_template(65536).ok();
        if chat_template.is_some() {
            tracing::info!("Loaded built-in chat template from model");
        } else {
            tracing::warn!("No built-in chat template — supply 'chat_template' per request");
        }

        #[cfg(feature = "mtmd")]
        let mtmd_ctx = match config.mmproj {
            Some(mmproj) => {
                tracing::info!("Loading mmproj: {}", mmproj.path.display());
                let ctx_params = MtmdContextParams::default()
                    .use_gpu(mmproj.use_gpu)
                    .n_threads(mmproj.n_threads);
                let ctx =
                    MtmdContext::init_from_file(&mmproj.path, &model, ctx_params).map_err(|e| {
                        anyhow::anyhow!("failed to load mmproj '{}': {e}", mmproj.path.display())
                    })?;
                tracing::info!(
                    "  vision={} audio={}",
                    ctx.supports_vision(),
                    ctx.supports_audio()
                );
                Some(ctx)
            }
            None => None,
        };

        Ok(Self {
            backend,
            model,
            chat_template,
            model_name,
            default_ctx_size: config.ctx_size,
            #[cfg(feature = "mtmd")]
            mtmd_ctx,
        })
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    pub fn n_ctx_train(&self) -> u32 {
        self.model.n_ctx_train()
    }

    pub fn n_embd(&self) -> i32 {
        self.model.n_embd()
    }

    pub fn default_ctx_size(&self) -> Option<NonZeroU32> {
        self.default_ctx_size
    }

    /// Effective context length reported to clients: the override if set,
    /// otherwise the model's trained context length.
    pub fn reported_ctx(&self) -> u32 {
        self.default_ctx_size
            .map_or_else(|| self.model.n_ctx_train(), NonZeroU32::get)
    }

    #[cfg(feature = "mtmd")]
    pub fn has_mtmd(&self) -> bool {
        self.mtmd_ctx.is_some()
    }

    #[cfg(not(feature = "mtmd"))]
    pub fn has_mtmd(&self) -> bool {
        false
    }

    /// Renders chat messages into a prompt using the request's template
    /// override, or the model's built-in template when none is given.
    pub fn render_prompt(
        &self,
        template_override: Option<&str>,
        messages: &[(String, String)],
    ) -> Result<String, InferenceError> {
        let chat_msgs = messages
            .iter()
            .map(|(role, content)| {
                LlamaChatMessage::new(role.clone(), content.clone()).map_err(|e| {
                    InferenceError::invalid(format!("invalid message (role={role}): {e}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let template = template_override
            .map(str::to_owned)
            .or_else(|| self.chat_template.clone());
        self.model
            .apply_chat_template(template.as_deref(), &chat_msgs, true)
            .map_err(|e| InferenceError::internal(format!("chat template: {e}")))
    }
}

/// The multimodal media marker for the current mtmd build, inserted where an
/// image or audio part appears in a message.
#[cfg(feature = "mtmd")]
pub fn mtmd_default_marker() -> String {
    MtmdContext::default_marker().to_string()
}
