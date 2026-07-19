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
without one. Run the full suite against a real model:

```sh
DLLM_TEST_MODEL=/path/to/model.gguf \
  cargo test -p dllm-daemon --test embedded_runtime_tests -- --nocapture
```

Build with an accelerator feature to exercise that backend:

```sh
# CUDA
DLLM_TEST_MODEL=/path/to/model.gguf \
  cargo test -p dllm-daemon --features cuda --test embedded_runtime_tests -- --nocapture
# Vulkan
DLLM_TEST_MODEL=/path/to/model.gguf \
  cargo test -p dllm-daemon --features vulkan --test embedded_runtime_tests -- --nocapture
```

The suite loads with `n_gpu_layers = 0`. To offload to the GPU for accelerator
runs, set `DLLMD_GPU_LAYERS` in the manual daemon run below rather than the unit
suite, or run the daemon end to end.

## Hardware matrix

Fill one row per backend as it is qualified. `Suite` is the conformance suite
result; `Inference` confirms real token generation on that backend (not just
device detection, per the Task 1 finding that an incompatible CUDA backend
enumerates devices and then aborts on first compute).

| Backend          | Host / GPU                    | Suite | Inference | Multi-GPU enumerated | NCCL | Notes |
|------------------|-------------------------------|-------|-----------|----------------------|------|-------|
| CPU              | (fill in)                     |       |           | n/a                  | n/a  |       |
| Vulkan           | (fill in)                     |       |           |                      | n/a  |       |
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
- CPU / Vulkan / CUDA evidence: pending a run on the appropriate hardware.
- Do not remove the sibling `dllm-llama-server` runtime (Task 8) until the
  conformance and lifecycle tests pass on the target backends and this matrix is
  filled in.
