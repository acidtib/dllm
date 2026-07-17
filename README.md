# DLLM - Distributed LLM Inference Network

DLLM is a peer-to-peer network for distributing LLM inference workloads across heterogeneous hardware. Each participating node runs `dllmd`, which manages inference, discovery, and encrypted forwarding without requiring a central coordination service.

## Quick Start

```sh
cargo build --release
cargo run --release --bin dllmd -- --help
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
```

## License

Apache-2.0
