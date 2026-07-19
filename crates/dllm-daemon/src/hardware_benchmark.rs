use dllm_daemon::{api, StoreError};
use dllm_protocol::{now_unix, CpuCapability, HardwareBenchmark, HardwareProfile};

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

struct MeasuredThroughput {
    prompt_tokens_per_second_milli: u64,
    decode_tokens_per_second_milli: u64,
}

const BENCHMARK_PROMPT: &str = "The quick brown fox jumps over the lazy dog. Describe what \
    happens next in exactly one sentence, staying factual and concise.";

async fn measure_benchmark(
    client: &reqwest::Client,
    runtime_url: &str,
) -> Result<MeasuredThroughput, Box<dyn std::error::Error>> {
    let tokenize: serde_json::Value = client
        .post(format!("{runtime_url}/tokenize"))
        .json(&serde_json::json!({ "content": BENCHMARK_PROMPT }))
        .send()
        .await?
        .json()
        .await?;
    let prompt_tokens = tokenize["tokens"].as_array().map_or(0, Vec::len) as u64;

    let prefill_start = std::time::Instant::now();
    client
        .post(format!("{runtime_url}/v1/chat/completions"))
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": BENCHMARK_PROMPT}],
            "max_tokens": 1,
            "stream": false,
        }))
        .send()
        .await?
        .error_for_status()?;
    let prefill_elapsed = prefill_start.elapsed().as_secs_f64();

    let decode_start = std::time::Instant::now();
    let decode_response: serde_json::Value = client
        .post(format!("{runtime_url}/v1/chat/completions"))
        .json(&serde_json::json!({
            "messages": [{"role": "user", "content": "Count from one to twenty."}],
            "max_tokens": 64,
            "stream": false,
        }))
        .send()
        .await?
        .json()
        .await?;
    let decode_elapsed = decode_start.elapsed().as_secs_f64();
    let completion_tokens = decode_response["usage"]["completion_tokens"]
        .as_u64()
        .unwrap_or(0);

    Ok(MeasuredThroughput {
        prompt_tokens_per_second_milli: if prefill_elapsed > 0.0 {
            (prompt_tokens as f64 / prefill_elapsed * 1000.0) as u64
        } else {
            0
        },
        decode_tokens_per_second_milli: if decode_elapsed > 0.0 {
            (completion_tokens as f64 / decode_elapsed * 1000.0) as u64
        } else {
            0
        },
    })
}

pub(crate) async fn benchmark_and_publish(
    state: api::ApiState,
    node_pubkey: [u8; 32],
    runtime_url: String,
    model_label: String,
    fit: dllm_runtime::FitReport,
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
    let measured = match measure_benchmark(&state.client, &runtime_url).await {
        Ok(measured) => measured,
        Err(error) => {
            eprintln!("hardware benchmark failed: {error}");
            return;
        }
    };
    let benchmark = HardwareBenchmark {
        model: model_label,
        backend: fit.backend,
        gpu_layers: fit.n_gpu_layers,
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

    #[tokio::test]
    async fn measure_benchmark_computes_positive_throughput_from_a_stub_server() {
        let app = axum::Router::new()
            .route(
                "/tokenize",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({ "tokens": [1, 2, 3, 4, 5] }))
                }),
            )
            .route(
                "/v1/chat/completions",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}],
                        "usage": {"prompt_tokens": 0, "completion_tokens": 8, "total_tokens": 8}
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let result = measure_benchmark(&client, &format!("http://{addr}"))
            .await
            .unwrap();
        assert!(result.prompt_tokens_per_second_milli > 0);
        assert!(result.decode_tokens_per_second_milli > 0);
    }
}
