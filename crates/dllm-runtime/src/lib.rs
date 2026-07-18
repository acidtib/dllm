use serde::{Deserialize, Serialize};
use std::{path::PathBuf, process::Stdio, time::Duration};
use thiserror::Error;
use tokio::{process::Child, time::Instant};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppConfig {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub host: String,
    pub port: u16,
    pub gpu_layers: String,
    pub context_size: u32,
    pub extra_args: Vec<String>,
}

impl LlamaCppConfig {
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--model".into(),
            self.model.display().to_string(),
            "--host".into(),
            self.host.clone(),
            "--port".into(),
            self.port.to_string(),
            "--n-gpu-layers".into(),
            self.gpu_layers.clone(),
            "--ctx-size".into(),
            self.context_size.to_string(),
        ];
        args.extend(self.extra_args.clone());
        args
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

/// Selects how the bundled `dllm-llama-server` binary should source its model.
///
/// This is a clap subcommand on the real CLI (`local <path>` or
/// `hf-model <repo> [quant]`), not a flag, so it is represented separately
/// from the leading flags in [`BundledRuntimeConfig::args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundledModelSource {
    Local(PathBuf),
    HuggingFace(String),
}

/// Configuration for spawning the workspace-bundled `dllm-llama-server`
/// binary, as opposed to [`LlamaCppConfig`] which targets an external stock
/// `llama-server` binary. The two have different CLI shapes (this one uses a
/// model-source subcommand instead of a `--model` flag) so they are kept as
/// distinct types rather than variants of one config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledRuntimeConfig {
    pub binary: PathBuf,
    pub model: BundledModelSource,
    pub host: String,
    pub port: u16,
    pub gpu_layers: u32,
    /// Context size override. `None` omits `--ctx-size` entirely so the
    /// bundled binary falls back to the model's native context length,
    /// matching its own `Option<NonZeroU32>` semantics.
    pub context_size: Option<u32>,
    pub api_key: Option<String>,
    pub parallel: usize,
    pub mmproj: Option<PathBuf>,
    /// When set, passed to the child process as `HF_HOME`, redirecting
    /// where `hf-hub` (inside the bundled binary) caches Hugging Face
    /// downloads. Ignored for `BundledModelSource::Local`.
    pub hf_home: Option<PathBuf>,
}

impl BundledRuntimeConfig {
    /// Builds the argument list in the order the bundled binary's clap
    /// parser requires: all top-level flags first, then the model-source
    /// subcommand and its arguments last. Passing the subcommand before a
    /// flag causes the real binary to reject the invocation at startup.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--host".into(),
            self.host.clone(),
            "--port".into(),
            self.port.to_string(),
            "--n-gpu-layers".into(),
            self.gpu_layers.to_string(),
        ];
        if let Some(context_size) = self.context_size {
            args.push("--ctx-size".into());
            args.push(context_size.to_string());
        }
        if let Some(api_key) = &self.api_key {
            args.push("--api-key".into());
            args.push(api_key.clone());
        }
        args.push("--parallel".into());
        args.push(self.parallel.to_string());
        if let Some(mmproj) = &self.mmproj {
            args.push("--mmproj".into());
            args.push(mmproj.display().to_string());
        }
        match &self.model {
            BundledModelSource::Local(path) => {
                args.push("local".into());
                args.push(path.display().to_string());
            }
            BundledModelSource::HuggingFace(repo) => {
                args.push("hf-model".into());
                args.push(repo.clone());
            }
        }
        args
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

pub struct RuntimeWorker {
    child: Child,
    endpoint: String,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime process error: {0}")]
    Process(#[from] std::io::Error),
    #[error("runtime did not become ready within {0:?}")]
    ReadinessTimeout(Duration),
    #[error("runtime exited before becoming ready with status {0}")]
    ProcessExited(std::process::ExitStatus),
}

impl RuntimeWorker {
    pub async fn start(config: &LlamaCppConfig, timeout: Duration) -> Result<Self, RuntimeError> {
        Self::spawn(&config.binary, config.args(), config.endpoint(), &[], timeout).await
    }

    pub async fn start_bundled(
        config: &BundledRuntimeConfig,
        timeout: Duration,
    ) -> Result<Self, RuntimeError> {
        let env: Vec<(String, String)> = config
            .hf_home
            .as_ref()
            .map(|path| ("HF_HOME".to_string(), path.display().to_string()))
            .into_iter()
            .collect();
        Self::spawn(&config.binary, config.args(), config.endpoint(), &env, timeout).await
    }

    async fn spawn(
        binary: &std::path::Path,
        args: Vec<String>,
        endpoint: String,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Self, RuntimeError> {
        let child = tokio::process::Command::new(binary)
            .args(args)
            .envs(env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut worker = Self { child, endpoint };
        if let Err(error) = worker.wait_ready(timeout).await {
            let _ = worker.terminate().await;
            return Err(error);
        }
        Ok(worker)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn shutdown(mut self) -> Result<(), RuntimeError> {
        self.terminate().await
    }

    async fn wait_ready(&mut self, timeout: Duration) -> Result<(), RuntimeError> {
        let client = reqwest::Client::new();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait()? {
                return Err(RuntimeError::ProcessExited(status));
            }
            if client
                .get(format!("{}/health", self.endpoint))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(RuntimeError::ReadinessTimeout(timeout))
    }

    async fn terminate(&mut self) -> Result<(), RuntimeError> {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            if tokio::time::timeout(Duration::from_secs(10), self.child.wait())
                .await
                .is_ok()
            {
                return Ok(());
            }
        }
        self.child.kill().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_command_is_reproducible() {
        let config = LlamaCppConfig {
            binary: "/opt/llama-server".into(),
            model: "/models/qwen.gguf".into(),
            host: "127.0.0.1".into(),
            port: 8081,
            gpu_layers: "38".into(),
            context_size: 2048,
            extra_args: vec!["--flash-attn".into(), "off".into()],
        };
        assert_eq!(
            config.args(),
            vec![
                "--model",
                "/models/qwen.gguf",
                "--host",
                "127.0.0.1",
                "--port",
                "8081",
                "--n-gpu-layers",
                "38",
                "--ctx-size",
                "2048",
                "--flash-attn",
                "off"
            ]
        );
        assert_eq!(config.endpoint(), "http://127.0.0.1:8081");
    }

    #[test]
    fn bundled_runtime_command_is_reproducible() {
        let local_config = BundledRuntimeConfig {
            binary: "/opt/dllm-llama-server".into(),
            model: BundledModelSource::Local("/models/qwen.gguf".into()),
            host: "127.0.0.1".into(),
            port: 8081,
            gpu_layers: 38,
            context_size: Some(2048),
            api_key: Some("secret".into()),
            parallel: 2,
            mmproj: Some("/models/mmproj.gguf".into()),
            hf_home: None,
        };
        assert_eq!(
            local_config.args(),
            vec![
                "--host",
                "127.0.0.1",
                "--port",
                "8081",
                "--n-gpu-layers",
                "38",
                "--ctx-size",
                "2048",
                "--api-key",
                "secret",
                "--parallel",
                "2",
                "--mmproj",
                "/models/mmproj.gguf",
                "local",
                "/models/qwen.gguf",
            ]
        );
        // Flags must precede the subcommand: the real clap parser rejects
        // flags that appear after `local`/`hf-model`.
        let args = local_config.args();
        let subcommand_index = args.iter().position(|arg| arg == "local").unwrap();
        assert!(
            args[..subcommand_index]
                .iter()
                .any(|arg| arg.starts_with("--")),
            "expected at least one flag before the subcommand"
        );
        assert!(
            args[subcommand_index + 1..]
                .iter()
                .all(|arg| !arg.starts_with("--")),
            "no flags may appear after the subcommand token"
        );
        assert_eq!(local_config.endpoint(), "http://127.0.0.1:8081");

        let hf_config = BundledRuntimeConfig {
            binary: "/opt/dllm-llama-server".into(),
            model: BundledModelSource::HuggingFace("unsloth/Qwen3.5-397B-A17B-GGUF".into()),
            host: "0.0.0.0".into(),
            port: 8082,
            gpu_layers: 0,
            context_size: None,
            api_key: None,
            parallel: 1,
            mmproj: None,
            hf_home: None,
        };
        assert_eq!(
            hf_config.args(),
            vec![
                "--host",
                "0.0.0.0",
                "--port",
                "8082",
                "--n-gpu-layers",
                "0",
                "--parallel",
                "1",
                "hf-model",
                "unsloth/Qwen3.5-397B-A17B-GGUF",
            ]
        );
        assert_eq!(hf_config.endpoint(), "http://0.0.0.0:8082");
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
