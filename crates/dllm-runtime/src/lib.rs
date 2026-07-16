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
        let child = tokio::process::Command::new(&config.binary)
            .args(config.args())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut worker = Self {
            child,
            endpoint: config.endpoint(),
        };
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
