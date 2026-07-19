//! Memory fitting: compute an achievable `n_gpu_layers` / `n_ctx` for a model
//! against available device memory.

use llama_cpp_4::prelude::*;
use std::path::{Path, PathBuf};

/// Result of a fit calculation. `backend` is supplied by the caller (discovery
/// in `dllmd`), not decided here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FitReport {
    pub n_gpu_layers: u32,
    pub n_ctx: u32,
    pub peak_memory_bytes: u64,
    pub backend: String,
}

/// Inputs for [`fit_model`].
#[derive(Debug, Clone)]
pub struct FitConfig {
    pub model_path: PathBuf,
    /// Minimum context size the fit keeps when memory is tight.
    pub n_ctx_min: u32,
    /// Free memory (bytes) to leave on each device after fitting.
    pub margin_bytes: usize,
    /// Backend label recorded in the report (e.g. "cuda", "cpu").
    pub backend_label: String,
}

/// Computes a [`FitReport`] for the model using the shared process backend.
pub fn fit_model(config: &FitConfig) -> anyhow::Result<FitReport> {
    let backend = crate::shared_backend()?;
    run_fit(
        backend,
        &config.model_path,
        config.n_ctx_min,
        config.margin_bytes,
        &config.backend_label,
    )
}

fn run_fit(
    backend: &LlamaBackend,
    model_path: &Path,
    n_ctx_min: u32,
    margin_bytes: usize,
    backend_label: &str,
) -> anyhow::Result<FitReport> {
    let margins = vec![margin_bytes; llama_cpp_4::max_devices()];
    let options = FitParams::default()
        .with_n_ctx_min(n_ctx_min)
        .with_margins(margins);
    let log_level = options.log_level;
    let fitted =
        fit_params(backend, model_path, options).map_err(|e| anyhow::anyhow!("fit failed: {e}"))?;
    let n_gpu_layers = fitted.model_params.n_gpu_layers().max(0) as u32;
    let n_ctx = fitted
        .context_params
        .n_ctx()
        .map(std::num::NonZeroU32::get)
        .unwrap_or(n_ctx_min);
    let memory = get_device_memory_data(
        model_path,
        &fitted.model_params,
        &fitted.context_params,
        log_level,
    )
    .map_err(|e| anyhow::anyhow!("device memory query failed: {e}"))?;
    let peak_memory_bytes = memory.entries.iter().map(|entry| entry.used() as u64).sum();
    Ok(FitReport {
        n_gpu_layers,
        n_ctx,
        peak_memory_bytes,
        backend: backend_label.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_report_serializes_expected_shape() {
        let report = FitReport {
            n_gpu_layers: 32,
            n_ctx: 4096,
            peak_memory_bytes: 4_200_000_000,
            backend: "cpu".to_string(),
        };
        let json: serde_json::Value = serde_json::to_value(&report).unwrap();
        assert_eq!(json["n_gpu_layers"], 32);
        assert_eq!(json["n_ctx"], 4096);
        assert_eq!(json["peak_memory_bytes"], 4_200_000_000u64);
        assert_eq!(json["backend"], "cpu");
    }
}
