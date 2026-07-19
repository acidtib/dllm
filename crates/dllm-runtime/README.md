# dllm-runtime

Inference runtime for the DLLM distributed inference network.

## Purpose

Manages llama.cpp child processes for local inference execution: the bundled
`dllm-llama-server` binary, or an external llama-server-compatible binary
configured via `DLLMD_RUNTIME_BIN`. Also shells out to `dllm-llama-server
--fit` to get an instant `gpu_layers`/`context_size` estimate before starting
a worker, used by `dllmd`'s hardware auto-tuning.
