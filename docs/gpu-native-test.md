# Embedded Runtime Backend Parity and GPU Evidence

Deliverable of Task 5 in
`docs/superpowers/plans/2026-07-18-universal-native-runtime.md`. It records the
backend conformance results for the embedded `dllmd` runtime across CPU and
accelerator backends.

The embedded runtime and the standalone `dllm-llama-server` share the same
`dllm-inference` core (the `openai` chat layer, generation, embeddings,
tokenization, and fit), so adapter parity is guaranteed by construction. This
document records that the shared core runs correctly on each backend and captures
the multi-GPU and NCCL evidence the plan requires.

## Conformance suite

The suite lives at `crates/dllm-daemon/tests/embedded_runtime_tests.rs`. It
covers chat completions (blocking and streaming), completions, embeddings,
tokenize/detokenize, model resolution, fit, error mapping, stream cancellation,
and bounded-wait timeout.

Three checks (error mapping, local model resolution, parameter validation) run
without a model. The rest are gated on `DLLM_TEST_MODEL` so CI stays green
without one. Run the model-backed suite single-threaded (llama.cpp contexts are
not thread-safe; the parallel harness segfaults on GPU backends):

```sh
DLLM_TEST_MODEL=/path/to/model.gguf \
  cargo test -p dllm-daemon --test embedded_runtime_tests -- --test-threads=1
```

The suite loads on CPU by default. To offload to the accelerator, build with the
backend feature and set `DLLM_TEST_GPU_LAYERS` (e.g. `99`):

```sh
# Vulkan
DLLM_TEST_MODEL=/path/to/model.gguf DLLM_TEST_GPU_LAYERS=99 \
  cargo test -p dllm-daemon --features vulkan --test embedded_runtime_tests -- --test-threads=1
# CUDA
DLLM_TEST_MODEL=/path/to/model.gguf DLLM_TEST_GPU_LAYERS=99 \
  cargo test -p dllm-daemon --features cuda --test embedded_runtime_tests -- --test-threads=1
```

## Hardware matrix

Fill one row per backend as it is qualified. `Suite` is the conformance suite
result; `Inference` confirms real token generation on that backend (not just
device detection, per the Task 1 finding that an incompatible CUDA backend
enumerates devices and then aborts on first compute).

| Backend          | Host / GPU                    | Suite | Inference | Multi-GPU enumerated | NCCL | Notes |
|------------------|-------------------------------|-------|-----------|----------------------|------|-------|
| CPU              | x86-64 (CachyOS)              | pass  | pass      | n/a                  | n/a  | Qwen2.5-0.5B-Instruct Q4_K_M, 12/12 tests in ~9s |
| Vulkan           | 2x GTX 1080                   | pass  | pass      | pass                 | n/a  | Vulkan 1.x; both GPUs enumerated, 25 layers split across Vulkan0/Vulkan1, 12/12 tests in ~5s, DLLM_TEST_GPU_LAYERS=99 |
| CUDA single-GPU  | (fill in)                     |       |           | n/a                  |      |       |
| CUDA multi-GPU   | (fill in)                     |       |           |                      |      |       |

Legend: use `pass` / `fail` / `n/a`. Record toolkit and driver versions in Notes.

## Multi-GPU and NCCL

The CUDA backend is built with NCCL (`GGML_CUDA_NCCL=ON`) per the Task 1
decision in `docs/universal-runtime-feasibility.md`. On a multi-GPU CUDA runner,
confirm:

- `ggml_cuda_init: found N CUDA devices` enumerates every GPU.
- Multi-GPU inference runs and uses NCCL collectives (not only single-GPU).
- `readelf -d` on the packaged `libggml-cuda.so` shows `libnccl.so.<major>` in
  NEEDED, and the main `dllmd` executable does not.

Record the commands and output here once run:

```
(paste ggml_cuda_init lines, a multi-GPU inference run, and the readelf output)
```

## Automatic fallback

Automatic backend fallback (selecting the next backend after an unavailable or
incompatible CUDA/Vulkan initialization) is implemented by the selection state
machine in `crates/dllm-runtime/src/backend.rs` and unit-tested there. Wiring it
to real device probing requires the ggml FFI probe (tracked as the Task 2
follow-up). Until then, `dllmd` reports the backend it was built for. Record
fallback evidence here once the probe lands:

```
(paste discovery output showing CUDA/Vulkan rejected and CPU selected)
```

## Status

- Conformance suite: added; runs on any host, model-backed cases gated on
  `DLLM_TEST_MODEL`.
- CPU: pass (12/12).
- Vulkan: pass (12/12) on 2x GTX 1080, including automatic multi-GPU layer split
  across both devices. NCCL is CUDA-only, so it is n/a for Vulkan.
- CUDA single-GPU and multi-GPU (with NCCL): pending a supported NVIDIA runner.
  CUDA 13.3 dropped Pascal (SM 6.1), so these GTX 1080s cannot run the release
  CUDA kernels (see docs/universal-runtime-feasibility.md); use a GPU in the
  release SM set or a CUDA 12.x toolkit.
- Do not remove the sibling `dllm-llama-server` runtime (Task 8) until the CUDA
  rows are filled in on a supported NVIDIA runner.
