use crate::embedded_runtime::{self, EmbeddedRuntime};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelSource {
    HuggingFace { repo: String, file: Option<String> },
    Local { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    Downloading,
    Downloaded,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRecord {
    pub id: String,
    pub source: ModelSource,
    pub artifact_path: Option<PathBuf>,
    pub status: ModelStatus,
    pub size_bytes: Option<u64>,
    pub added_at_unix: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredLibrary {
    models: BTreeMap<String, ModelRecord>,
}

pub struct ModelLibrary {
    path: PathBuf,
    models: BTreeMap<String, ModelRecord>,
}

impl ModelLibrary {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let mut models = if path.exists() {
            serde_json::from_slice::<StoredLibrary>(&fs::read(&path)?)?.models
        } else {
            BTreeMap::new()
        };
        let mut interrupted = false;
        for record in models.values_mut() {
            if matches!(
                record.status,
                ModelStatus::Downloading | ModelStatus::Loading
            ) {
                record.status = ModelStatus::Failed;
                record.last_error =
                    Some("model acquisition was interrupted; retry model add".into());
                interrupted = true;
            }
        }
        let library = Self { path, models };
        if interrupted {
            library.save()?;
        }
        Ok(library)
    }

    pub fn list(&self) -> Vec<ModelRecord> {
        self.models.values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<&ModelRecord> {
        self.models.get(id)
    }

    pub fn put(&mut self, record: ModelRecord) -> anyhow::Result<()> {
        self.models.insert(record.id.clone(), record);
        self.save()
    }

    pub fn remove(&mut self, id: &str) -> anyhow::Result<Option<ModelRecord>> {
        let removed = self.models.remove(id);
        if removed.is_some() {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> anyhow::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(
            &temporary,
            &StoredLibrary {
                models: self.models.clone(),
            },
        )?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

pub async fn load_model(
    source: ModelSource,
    model_id: String,
) -> anyhow::Result<(
    Arc<EmbeddedRuntime>,
    PathBuf,
    Option<dllm_runtime::FitReport>,
    u32,
    u32,
)> {
    let inference_source = match source {
        ModelSource::HuggingFace { repo, file } => {
            dllm_inference::ModelSource::HuggingFace { repo, model: file }
        }
        ModelSource::Local { path } => dllm_inference::ModelSource::Local(path),
    };
    let resolved_path =
        tokio::task::spawn_blocking(move || inference_source.resolve_noninteractive()).await??;
    let fit_path = resolved_path.clone();
    let fit = tokio::task::spawn_blocking(move || {
        dllm_inference::fit_model(&dllm_inference::FitConfig {
            model_path: fit_path,
            n_ctx_min: 4096,
            margin_bytes: 1_073_741_824,
            backend_label: embedded_runtime::active_backend().to_string(),
        })
    })
    .await?
    .ok();
    let gpu_layers = std::env::var("DLLMD_GPU_LAYERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| fit.as_ref().map(|report| report.n_gpu_layers))
        .unwrap_or(38);
    let context_size = std::env::var("DLLMD_CONTEXT_SIZE")
        .ok()
        .and_then(|value| value.parse().ok())
        .or_else(|| fit.as_ref().map(|report| report.n_ctx))
        .unwrap_or(2048);
    let load_path = resolved_path.clone();
    let engine = tokio::task::spawn_blocking(move || {
        dllm_inference::InferenceModel::load(dllm_inference::ModelConfig {
            model_path: load_path,
            n_gpu_layers: gpu_layers,
            ctx_size: NonZeroU32::new(context_size),
            #[cfg(feature = "mtmd")]
            mmproj: std::env::var("DLLMD_MMPROJ_PATH").ok().map(|path| {
                dllm_inference::MmprojConfig {
                    path: path.into(),
                    use_gpu: true,
                    n_threads: 4,
                }
            }),
        })
    })
    .await??;
    Ok((
        Arc::new(EmbeddedRuntime::new(
            engine,
            1,
            embedded_runtime::active_backend(),
            model_id,
        )),
        resolved_path,
        fit.map(|report| dllm_runtime::FitReport {
            n_gpu_layers: report.n_gpu_layers,
            n_ctx: report.n_ctx,
            peak_memory_bytes: report.peak_memory_bytes,
            backend: report.backend,
        }),
        gpu_layers,
        context_size,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_persists_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.json");
        let mut library = ModelLibrary::load(path.clone()).unwrap();
        library
            .put(ModelRecord {
                id: "test".into(),
                source: ModelSource::Local {
                    path: "model.gguf".into(),
                },
                artifact_path: Some("model.gguf".into()),
                status: ModelStatus::Ready,
                size_bytes: Some(42),
                added_at_unix: 1,
                last_error: None,
            })
            .unwrap();
        let loaded = ModelLibrary::load(path).unwrap();
        assert_eq!(loaded.get("test").unwrap().size_bytes, Some(42));
    }

    #[test]
    fn interrupted_add_becomes_retryable_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.json");
        let mut library = ModelLibrary::load(path.clone()).unwrap();
        library
            .put(ModelRecord {
                id: "test".into(),
                source: ModelSource::HuggingFace {
                    repo: "Qwen/test-GGUF".into(),
                    file: None,
                },
                artifact_path: None,
                status: ModelStatus::Loading,
                size_bytes: None,
                added_at_unix: 1,
                last_error: None,
            })
            .unwrap();
        let loaded = ModelLibrary::load(path).unwrap();
        let record = loaded.get("test").unwrap();
        assert_eq!(record.status, ModelStatus::Failed);
        assert!(record.last_error.as_deref().unwrap().contains("retry"));
    }
}
