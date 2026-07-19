use dllm_daemon::embedded_runtime::EmbeddedRuntime;
use dllm_daemon::{api, StoreError};
use dllm_protocol::{now_unix, CpuCapability, HardwareBenchmark, HardwareProfile};
use std::sync::Arc;

fn merge_benchmark_into_profile(
    existing: Option<HardwareProfile>,
    node_pubkey: [u8; 32],
    benchmark: HardwareBenchmark,
) -> HardwareProfile {
    let mut profile = existing.unwrap_or_else(|| HardwareProfile {
        node_pubkey,
        observed_at_unix: 0,
        cpu: CpuCapability {
            model: String::new(),
            physical_cores: 0,
            logical_cores: 0,
            features: vec![],
        },
        system_memory_bytes: 0,
        available_memory_bytes: 0,
        accelerators: vec![],
        runtimes: vec![],
        benchmarks: vec![],
    });
    profile.observed_at_unix = now_unix();
    profile.benchmarks.retain(|candidate| {
        !(candidate.model == benchmark.model && candidate.backend == benchmark.backend)
    });
    profile.benchmarks.push(benchmark);
    profile
}

pub(crate) async fn benchmark_and_publish(
    state: api::ApiState,
    node_pubkey: [u8; 32],
    engine: Arc<EmbeddedRuntime>,
    model_label: String,
    fit: dllm_runtime::FitReport,
    gpu_layers: u32,
    context_size: u32,
) {
    let already_benchmarked = state
        .store
        .lock()
        .await
        .state
        .state
        .hardware_profiles
        .iter()
        .find(|profile| profile.node_pubkey == node_pubkey)
        .is_some_and(|profile| {
            profile
                .benchmarks
                .iter()
                .any(|benchmark| benchmark.model == model_label && benchmark.backend == fit.backend)
        });
    if already_benchmarked {
        return;
    }
    let measured = match engine.measure_throughput().await {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("hardware benchmark failed: {error}");
            return;
        }
    };
    let benchmark = HardwareBenchmark {
        model: model_label,
        backend: fit.backend,
        gpu_layers,
        context_size,
        concurrency: 1,
        prompt_tokens_per_second_milli: measured.prompt_tokens_per_second_milli,
        decode_tokens_per_second_milli: measured.decode_tokens_per_second_milli,
        // Pre-flight projection from fit_params/get_device_memory_data, not a
        // measurement of the running worker: measure_benchmark only measures
        // throughput, and the worker exposes no live memory query.
        peak_memory_bytes: fit.peak_memory_bytes,
    };
    let mut store = state.store.lock().await;
    let existing = store
        .state
        .state
        .hardware_profiles
        .iter()
        .find(|profile| profile.node_pubkey == node_pubkey)
        .cloned();
    let profile = merge_benchmark_into_profile(existing, node_pubkey, benchmark);
    match store.publish_hardware_profile(profile) {
        Ok(true) => {
            if let Err(error) = store.save(&state.state_path) {
                eprintln!("failed to save hardware profile: {error}");
            }
        }
        Ok(false) => {}
        Err(StoreError::OwnerAuthorityUnavailable) => {
            println!(
                "hardware benchmark complete; publishing to network state requires the \
                 network owner, skipped on this member node"
            );
        }
        Err(error) => eprintln!("failed to publish hardware profile: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_benchmark_into_profile_replaces_matching_entry_and_keeps_others() {
        let node_pubkey = [7u8; 32];
        let existing = dllm_protocol::HardwareProfile {
            node_pubkey,
            observed_at_unix: 1,
            cpu: dllm_protocol::CpuCapability {
                model: "operator-reported cpu".into(),
                physical_cores: 8,
                logical_cores: 16,
                features: vec![],
            },
            system_memory_bytes: 32_000_000_000,
            available_memory_bytes: 20_000_000_000,
            accelerators: vec![],
            runtimes: vec![],
            benchmarks: vec![dllm_protocol::HardwareBenchmark {
                model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
                backend: "vulkan".into(),
                gpu_layers: 18,
                context_size: 2048,
                concurrency: 1,
                prompt_tokens_per_second_milli: 1_000,
                decode_tokens_per_second_milli: 500,
                peak_memory_bytes: 1,
            }],
        };
        let new_benchmark = dllm_protocol::HardwareBenchmark {
            model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
            backend: "cuda".into(),
            gpu_layers: 32,
            context_size: 8192,
            concurrency: 1,
            prompt_tokens_per_second_milli: 9_000,
            decode_tokens_per_second_milli: 4_000,
            peak_memory_bytes: 4_200_000_000,
        };
        let merged =
            merge_benchmark_into_profile(Some(existing), node_pubkey, new_benchmark.clone());
        assert_eq!(merged.cpu.model, "operator-reported cpu");
        assert_eq!(merged.benchmarks.len(), 2);
        assert!(merged.benchmarks.contains(&new_benchmark));

        let replacement = dllm_protocol::HardwareBenchmark {
            backend: "vulkan".into(),
            decode_tokens_per_second_milli: 600,
            ..new_benchmark.clone()
        };
        let merged_again =
            merge_benchmark_into_profile(Some(merged), node_pubkey, replacement.clone());
        assert_eq!(merged_again.benchmarks.len(), 2);
        assert!(merged_again.benchmarks.contains(&replacement));
        assert!(!merged_again
            .benchmarks
            .iter()
            .any(|b| b.backend == "vulkan" && b.decode_tokens_per_second_milli == 500));
    }

    #[test]
    fn merge_benchmark_into_profile_creates_fresh_profile_when_none_exists() {
        let node_pubkey = [9u8; 32];
        let benchmark = dllm_protocol::HardwareBenchmark {
            model: "unsloth/Qwen3.5-397B-A17B-GGUF".into(),
            backend: "cuda".into(),
            gpu_layers: 32,
            context_size: 8192,
            concurrency: 1,
            prompt_tokens_per_second_milli: 9_000,
            decode_tokens_per_second_milli: 4_000,
            peak_memory_bytes: 4_200_000_000,
        };

        let profile = merge_benchmark_into_profile(None, node_pubkey, benchmark.clone());

        assert_eq!(profile.node_pubkey, node_pubkey);
        assert_eq!(profile.benchmarks, vec![benchmark]);
        assert_eq!(profile.cpu.model, "");
        assert_eq!(profile.system_memory_bytes, 0);
        assert!(profile.observed_at_unix > 0);
    }
}
