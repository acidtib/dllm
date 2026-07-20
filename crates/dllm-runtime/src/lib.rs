use serde::{Deserialize, Serialize};

pub mod backend;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub id: String,
    pub architecture: String,
    pub quantization: String,
    pub artifact_revision: String,
    pub artifact_sha256: Vec<String>,
    pub context_size: u32,
}

/// Result of a memory-fit pass: how many layers fit on the accelerator, the
/// context length chosen, and the backend it was measured on. Produced
/// in-process by `dllm-inference` and recorded in the node's hardware profile.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FitReport {
    pub n_gpu_layers: u32,
    pub n_ctx: u32,
    pub peak_memory_bytes: u64,
    pub backend: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_report_parses_from_json() {
        let report: FitReport = serde_json::from_str(
            r#"{"n_gpu_layers":32,"n_ctx":4096,"peak_memory_bytes":4200000000,"backend":"cuda"}"#,
        )
        .unwrap();
        assert_eq!(report.n_gpu_layers, 32);
        assert_eq!(report.n_ctx, 4096);
        assert_eq!(report.peak_memory_bytes, 4_200_000_000);
        assert_eq!(report.backend, "cuda");
    }

    #[test]
    fn phase_zero_qwen_manifest_parses() {
        let manifest: ModelManifest = serde_yaml::from_str(include_str!(
            "../../../manifests/qwen2.5-14b-instruct-q4_k_m.yaml"
        ))
        .unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.architecture, "qwen2");
        assert_eq!(manifest.quantization, "Q4_K_M");
        assert_eq!(manifest.artifact_sha256.len(), 3);
    }

    #[test]
    fn phase_two_gemma_manifest_parses() {
        let manifest: ModelManifest =
            serde_yaml::from_str(include_str!("../../../manifests/gemma-3-1b-it-q4_k_m.yaml"))
                .unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.architecture, "gemma3");
        assert_eq!(manifest.quantization, "Q4_K_M");
        assert_eq!(manifest.artifact_sha256.len(), 1);
    }
}
