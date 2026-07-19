# dllm-llama-server

Bundled OpenAI-compatible inference server for the DLLM distributed inference
network, vendored from `llama-cpp-rs`.

## Binary

Installed as `dllm-llama-server`. Spawned automatically by `dllmd` as a child
process; not a binary users install or invoke directly.

## Purpose

Serves a model, local file or downloaded from Hugging Face, over an
OpenAI-compatible HTTP API: `/v1/chat/completions`, `/v1/completions`,
`/v1/embeddings`, `/v1/models`, tokenize/detokenize, and file upload
endpoints.

## Features

- CPU backend by default; `cuda`, `metal`, and `vulkan` Cargo features select
  a GPU backend
- `mtmd` (multimodal) support compiled in by default from upstream
  `llama-cpp-rs`; not exposed or supported as a DLLM feature
- `--fit` mode: reuses `llama-cpp-4`'s own device-memory query and layer-fitting
  logic to report an instant `gpu_layers`/`context_size` estimate for a model
  on this backend, without starting the server. Used by `dllmd`'s hardware
  auto-tuning (see `dllm-runtime`).
