# DLLM - Distributed LLM Inference Network

DLLM is a peer-to-peer network for distributing LLM inference workloads across heterogeneous hardware. Each participating node runs `dllmd`, which manages inference, discovery, and encrypted forwarding without requiring a central coordination service.

## Quick Start

```sh
cargo build --release
cargo run --release --bin dllmd -- --help
```

See [`docs/getting-started.md`](docs/getting-started.md) for a full walkthrough:
Docker quick start, downloading a model, onboarding a second node, and sending
your first chat completion request.

## Web UI

The React web app lives in `apps/web/` and is managed as part of the root Bun workspace.

```sh
bun install
bun run --cwd apps/web build
```

## Monorepo Tasks

[Mise](https://mise.jdx.dev/) orchestrates the whole repository. After installing Mise, run:

```sh
mise install
mise run fmt
mise run lint
mise run test
mise run build
mise run ci
```

Development shortcuts:

```sh
mise run dev:web              # Start the web app dev server
mise run dev:daemon           # Run the dllmd daemon
mise run dev:cli              # Run the dllm CLI
mise run dev:gpu-nodes        # Start the owner GPU dev node (docker compose, GPU 0)
mise run dev:gpu-image:build  # Build the CUDA dev image locally
mise run dev:gpu-nodes:local  # Start the GPU dev node from the local CUDA image
mise run dev:gpu-nodes:down   # Stop the GPU dev nodes
```

## Architecture

- `dllmd` is the only service users install and operate
- Nodes discover each other via a libp2p peer network
- Owner-signed state governs membership, policy, and transport bindings
- No SSH or dedicated relay required

## Workspace Crates

| Crate | Purpose |
|-------|---------|
| `dllm-protocol` | Shared types: network state, identity, tokens |
| `dllmd` | Node daemon: API server, credentials, inference |
| `dllm-cli` | CLI client (binary: `dllm`) |
| `dllm-runtime` | Inference runtime for llama.cpp |
| `dllm-transport` | libp2p peer transport layer |
| `dllm-probe` | Standalone libp2p diagnostic tool (binary: `dllm-dev-probe`) |
| `dllm-llama-server` | Bundled OpenAI-compatible llama.cpp server, spawned automatically by `dllmd` |

## Development

```sh
cargo fmt --all
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
bun run --cwd apps/web build
```

## Docker

Build a `dllmd` image from `docker/Dockerfile.dllmd`:

```sh
docker build -f docker/Dockerfile.dllmd -t dllmd:latest .
```

The Dockerfile builds both the `dllmd` and `dllm` binaries and packages `dllmd` in a minimal Debian runtime image. It supports a CPU runtime target by default and a `runtime-cuda` target for GPU nodes, built against a separate base image (`docker/Dockerfile.cuda-runtime`) that bundles the matching CUDA runtime libraries:

```sh
docker build -f docker/Dockerfile.cuda-runtime --build-arg CUDA_ARCHITECTURES=61 -t dllm:cuda-runtime-sm61-v1 .
docker build -f docker/Dockerfile.dllmd --target runtime-cuda --build-arg CUDA_RUNTIME_IMAGE=dllm:cuda-runtime-sm61-v1 -t dllm:cuda-sm61 .
```

Prebuilt CPU and CUDA images are published by the `Docker images` and `CUDA runtime images` GitHub Actions workflows. `docker-compose.dev-gpu.yml` runs local GPU dev nodes against either the published or a locally built image; see the `mise run dev:gpu-nodes*` tasks above.

## License

Apache-2.0
