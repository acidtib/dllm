# DLLM - Distributed LLM Inference Network

DLLM is a peer-to-peer network for distributing LLM inference workloads across heterogeneous hardware. Each participating node runs `dllmd`, which manages inference, discovery, and encrypted forwarding without requiring a central coordination service.

## Quick Start

```sh
cargo build --release
cargo run --release --bin dllmd -- --help
```

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
mise run dev:web     # Start the web app dev server
mise run dev:daemon  # Run the dllmd daemon
mise run dev:cli     # Run the dllm CLI
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

The Dockerfile builds both the `dllmd` and `dllm` binaries and packages `dllmd` in a minimal Debian runtime image.

## License

Apache-2.0
