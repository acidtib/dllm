# DLLM release archive

This archive contains a prebuilt `dllm` (CLI) and `dllmd` (daemon), the only
two binaries you need to install and run.

## Contents

- `dllmd` - the node daemon: API server, inference, discovery, and
  encrypted forwarding, all in-process
- `dllm` - the CLI client
- `LICENSE.llama-cpp-rs` - attribution for vendored llama.cpp source
- this file

## Running

```sh
./dllmd
```

`dllmd` binds `127.0.0.1:7337` by default (override with `DLLMD_BIND`), and
self-bootstraps as the owner of a fresh single-node network on first start.
Keys and state persist under `~/.dllm/`.

To join an existing network instead of starting a new one:

```sh
./dllm onboard <authority-url>
```

Then, from another terminal, point the CLI at the running daemon:

```sh
./dllm --help
```

## Variants

This archive is built for one hardware variant (CPU, Vulkan, or CUDA); the
filename indicates which. Use the CPU build unless you have a supported GPU.

## Full documentation

The full getting-started guide (Docker quick start, downloading a model,
onboarding a second node, sending a chat completion request) lives in the
main repository: https://github.com/acidtib/dllm/blob/main/docs/getting-started.md

## License

DLLM's own license is in the main repository. `LICENSE.llama-cpp-rs`, bundled
in this archive, covers vendored llama.cpp source included in the binaries.
