//! Model source resolution: local paths and Hugging Face downloads, plus
//! multimodal projector auto-detection.

use anyhow::Context as _;
use hf_hub::api::sync::{Api, ApiBuilder};
use std::path::PathBuf;

/// Hugging Face client pointed at `~/.dllm/models`, so downloaded GGUFs land
/// alongside the rest of DLLM's per-user files rather than the default HF
/// cache. Falls back to the hf-hub default cache when the home directory
/// can't be resolved.
fn hf_api_builder() -> ApiBuilder {
    let builder = ApiBuilder::new().with_progress(true);
    match dirs::home_dir() {
        Some(home) => builder.with_cache_dir(home.join(".dllm").join("models")),
        None => builder,
    }
}

/// Where a model comes from. The CLI maps its `local` / `hf-model` subcommands
/// onto this; the daemon constructs it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    Local(PathBuf),
    HuggingFace { repo: String, model: Option<String> },
}

impl ModelSource {
    /// Resolves to an absolute local GGUF path, downloading from Hugging Face
    /// when needed.
    pub fn resolve(self) -> anyhow::Result<PathBuf> {
        self.resolve_with_prompt(true)
    }

    /// Resolves without reading stdin, selecting the preferred available GGUF
    /// group when no exact model was requested.
    pub fn resolve_noninteractive(self) -> anyhow::Result<PathBuf> {
        self.resolve_with_prompt(false)
    }

    fn resolve_with_prompt(self, allow_prompt: bool) -> anyhow::Result<PathBuf> {
        match self {
            ModelSource::Local(path) => Ok(path),
            ModelSource::HuggingFace { repo, model } => {
                let api = hf_api_builder()
                    .build()
                    .context("failed to build HF API client")?;
                resolve_hf(&api, &repo, model, allow_prompt)
            }
        }
    }
}

const QUANT_PREFERENCE: &[&str] = &[
    "Q4_K_M", "Q4_K_S", "Q4_0", "Q5_K_M", "Q5_K_S", "Q5_0", "Q3_K_M", "Q3_K_S", "Q8_0", "Q6_K",
    "Q2_K", "IQ4_XS", "IQ3_M",
];

#[derive(Debug)]
struct ModelGroup {
    label: String,
    files: Vec<String>,
}

impl ModelGroup {
    fn preference_score(&self) -> usize {
        QUANT_PREFERENCE
            .iter()
            .position(|q| self.label.to_uppercase().contains(q))
            .unwrap_or(usize::MAX)
    }
}

fn collect_groups(all_ggufs: Vec<String>) -> Vec<ModelGroup> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in all_ggufs {
        let key = if let Some(slash) = path.find('/') {
            path[..slash].to_string()
        } else {
            let stem = path.trim_end_matches(".gguf");
            if let Some(of_pos) = stem.rfind("-of-") {
                let before_of = &stem[..of_pos];
                if let Some(dash) = before_of.rfind('-') {
                    let shard_num = &before_of[dash + 1..];
                    if shard_num.chars().all(|c| c.is_ascii_digit()) {
                        before_of[..dash].to_string()
                    } else {
                        stem.to_string()
                    }
                } else {
                    stem.to_string()
                }
            } else {
                stem.to_string()
            }
        };
        map.entry(key).or_default().push(path);
    }
    map.into_iter()
        .map(|(key, mut files)| {
            files.sort();
            let shard_info = if files.len() > 1 {
                format!("  [{} shards]", files.len())
            } else {
                String::new()
            };
            ModelGroup {
                label: format!("{key}{shard_info}"),
                files,
            }
        })
        .collect()
}

fn prompt_user(groups: &[ModelGroup]) -> anyhow::Result<usize> {
    use std::io::{self, IsTerminal as _, Write};
    eprintln!("\nAvailable models in repo:");
    for (i, g) in groups.iter().enumerate() {
        eprintln!("  {:>2})  {}", i + 1, g.label);
    }
    if !io::stdin().is_terminal() {
        let best = groups
            .iter()
            .enumerate()
            .min_by_key(|(_, g)| g.preference_score())
            .map_or(0, |(i, _)| i);
        eprintln!("\nNon-interactive — auto-selected: {}", groups[best].label);
        return Ok(best);
    }
    loop {
        eprint!("\nSelect a model [1–{}]: ", groups.len());
        io::stderr().flush().ok();
        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        match line.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= groups.len() => return Ok(n - 1),
            _ => eprintln!("  Enter a number between 1 and {}.", groups.len()),
        }
    }
}

fn resolve_hf(
    api: &Api,
    repo: &str,
    model: Option<String>,
    allow_prompt: bool,
) -> anyhow::Result<PathBuf> {
    let api_repo = api.model(repo.to_string());
    // Exact .gguf filename → download directly.
    if let Some(ref filename) = model {
        if filename.ends_with(".gguf") {
            return api_repo
                .get(filename)
                .with_context(|| format!("failed to download '{filename}' from '{repo}'"));
        }
    }
    let info = api_repo
        .info()
        .with_context(|| format!("failed to fetch repo info for '{repo}'"))?;
    let all_ggufs: Vec<String> = info
        .siblings
        .into_iter()
        .map(|s| s.rfilename)
        .filter(|n| n.ends_with(".gguf"))
        .collect();
    if all_ggufs.is_empty() {
        anyhow::bail!("no .gguf files found in repo '{repo}'");
    }
    let groups = collect_groups(all_ggufs);
    let chosen_idx = if let Some(filter) = model {
        let filter_up = filter.to_uppercase();
        groups
            .iter()
            .position(|g| {
                let label_key = g.label.split_whitespace().next().unwrap_or(&g.label);
                label_key.to_uppercase() == filter_up
                    || label_key.to_uppercase().contains(&filter_up)
            })
            .with_context(|| {
                let available: Vec<_> = groups
                    .iter()
                    .map(|g| {
                        g.label
                            .split_whitespace()
                            .next()
                            .unwrap_or(&g.label)
                            .to_string()
                    })
                    .collect();
                format!(
                    "no group matching '{filter}' in '{repo}'. Available: {}",
                    available.join(", ")
                )
            })?
    } else if groups.len() == 1 {
        eprintln!("Auto-selected: {}", groups[0].label);
        0
    } else if allow_prompt {
        prompt_user(&groups)?
    } else {
        let best = groups
            .iter()
            .enumerate()
            .min_by_key(|(_, group)| group.preference_score())
            .map_or(0, |(index, _)| index);
        eprintln!("Auto-selected: {}", groups[best].label);
        best
    };
    let group = &groups[chosen_idx];
    eprintln!("\nDownloading: {}", group.label);
    let mut first_path: Option<PathBuf> = None;
    for (i, file) in group.files.iter().enumerate() {
        if group.files.len() > 1 {
            eprintln!("  shard {}/{}: {file}", i + 1, group.files.len());
        }
        let path = api
            .model(repo.to_string())
            .get(file)
            .with_context(|| format!("failed to download shard '{file}'"))?;
        if first_path.is_none() {
            first_path = Some(path);
        }
    }
    first_path.ok_or_else(|| anyhow::anyhow!("no files downloaded"))
}

// ---------------------------------------------------------------------------
// mmproj auto-detection and download
// ---------------------------------------------------------------------------

/// Preference order when multiple mmproj files are found. Earlier entries win.
#[cfg(feature = "mtmd")]
const MMPROJ_PREFER: &[&str] = &[
    "-F16.gguf",
    "-f16.gguf",
    "-BF16.gguf",
    "-bf16.gguf",
    "-F32.gguf",
    "-f32.gguf",
];

/// Try to download the best mmproj GGUF from a Hugging Face repo. Lists the
/// repo's files, picks the best `mmproj-*.gguf` by [`MMPROJ_PREFER`], downloads
/// (or retrieves from local cache) and returns its path.
#[cfg(feature = "mtmd")]
pub fn download_mmproj_from_hf(repo: &str) -> Option<PathBuf> {
    let api = match hf_api_builder().build() {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("Could not build HF API client for mmproj lookup: {e}");
            return None;
        }
    };
    let api_repo = api.model(repo.to_string());

    let info = match api_repo.info() {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("Could not fetch repo info for '{repo}': {e}");
            return None;
        }
    };

    let mut candidates: Vec<String> = info
        .siblings
        .into_iter()
        .map(|s| s.rfilename)
        .filter(|name| name.starts_with("mmproj") && name.ends_with(".gguf"))
        .collect();

    if candidates.is_empty() {
        tracing::warn!(
            "No mmproj-*.gguf files found in repo '{repo}'. \
             The repo may not include a vision projector."
        );
        return None;
    }

    candidates.sort_by(|a, b| {
        let score = |name: &str| {
            MMPROJ_PREFER
                .iter()
                .position(|suf| name.ends_with(suf))
                .unwrap_or(MMPROJ_PREFER.len())
        };
        score(a).cmp(&score(b)).then_with(|| a.cmp(b))
    });

    let chosen = &candidates[0];
    tracing::info!(
        "Downloading mmproj '{chosen}' from '{repo}'{}…",
        if candidates.len() > 1 {
            format!(
                " ({} candidates; use --mmproj to override)",
                candidates.len()
            )
        } else {
            String::new()
        }
    );

    match api_repo.get(chosen) {
        Ok(path) => {
            tracing::info!("mmproj cached at: {}", path.display());
            Some(path)
        }
        Err(e) => {
            tracing::warn!("Failed to download '{chosen}' from '{repo}': {e}");
            None
        }
    }
}

/// Scan `dir` for `mmproj*.gguf` files and return the best match by
/// [`MMPROJ_PREFER`], or the first alphabetically. Skips silently if the
/// directory cannot be read.
#[cfg(feature = "mtmd")]
pub fn find_mmproj_in_dir(dir: &std::path::Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("gguf")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("mmproj"))
                    .unwrap_or(false)
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|a, b| {
        let score = |p: &PathBuf| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            MMPROJ_PREFER
                .iter()
                .position(|suf| name.ends_with(suf))
                .unwrap_or(MMPROJ_PREFER.len())
        };
        score(a).cmp(&score(b)).then_with(|| a.cmp(b))
    });

    let chosen = candidates.remove(0);
    if !candidates.is_empty() {
        tracing::info!(
            "Auto-detected mmproj: {} ({} other candidate(s) in same dir; \
             use --mmproj to override)",
            chosen.display(),
            candidates.len()
        );
    } else {
        tracing::info!("Auto-detected mmproj: {}", chosen.display());
    }
    Some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_plain_gguf() {
        let files = vec!["model.Q4_K_M.gguf".to_string()];
        let groups = collect_groups(files);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 1);
    }

    #[test]
    fn sharded_flat_files_grouped() {
        let files = vec![
            "model-Q4_K_M-00001-of-00003.gguf".to_string(),
            "model-Q4_K_M-00002-of-00003.gguf".to_string(),
            "model-Q4_K_M-00003-of-00003.gguf".to_string(),
        ];
        let groups = collect_groups(files);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 3);
        assert_eq!(groups[0].files[0], "model-Q4_K_M-00001-of-00003.gguf");
    }

    #[test]
    fn subdirectory_files_grouped_by_dir() {
        let files = vec![
            "Q4_K_M/model-00001-of-00006.gguf".to_string(),
            "Q4_K_M/model-00002-of-00006.gguf".to_string(),
            "Q3_K_M/model-00001-of-00005.gguf".to_string(),
            "Q3_K_M/model-00002-of-00005.gguf".to_string(),
        ];
        let groups = collect_groups(files);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].label, "Q3_K_M  [2 shards]");
        assert_eq!(groups[1].label, "Q4_K_M  [2 shards]");
    }

    #[test]
    fn mixed_quants_each_get_own_group() {
        let files = vec![
            "llama-Q4_K_M.gguf".to_string(),
            "llama-Q8_0.gguf".to_string(),
        ];
        let groups = collect_groups(files);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn preference_score_orders_correctly() {
        let files = vec![
            "Q8_0/model.gguf".to_string(),
            "Q4_K_M/model.gguf".to_string(),
            "Q3_K_S/model.gguf".to_string(),
        ];
        let groups = collect_groups(files);
        let mut scores: Vec<_> = groups
            .iter()
            .map(|g| (g.preference_score(), &g.label))
            .collect();
        scores.sort();
        assert!(scores[0].1.contains("Q4_K_M"), "got {scores:?}");
    }

    #[test]
    fn local_source_resolves_to_same_path() {
        let source = ModelSource::Local(PathBuf::from("/models/qwen.gguf"));
        assert_eq!(
            source.resolve().unwrap(),
            PathBuf::from("/models/qwen.gguf")
        );
    }
}
