## Writing style

- Do not use emojis anywhere: code, comments, commit messages, or chat replies.
- Do not use em-dashes. Use commas, colons, parentheses, or separate sentences.
- Avoid filler "LLM-tell" phrasing. Write plainly and directly.

## Code comments

- Comment to explain why something is done or to flag a non-obvious constraint.
- Do not write summary comments that just restate what the next line does.
- Skip section-header and narration comments. Let the code speak for itself.

## Git

- Never add a co-author trailer to commits (no "Co-Authored-By" line).
- Keep commit messages short and factual.
- Preserve unrelated and user-owned working-tree changes.

## Project architecture

- `dllmd` (daemon) and `dllm` (CLI) are the only binaries users must install
  and operate. `dllm-dev-probe` is a diagnostic tool, not for production use.
- `dllm-llama-server` is the bundled inference runtime `dllmd` spawns
  automatically; it is not a binary users install or invoke directly.
- Do not introduce a dedicated relay, discovery, bootstrap, or coordination
  service as a requirement.
- Ordinary participating `dllmd` nodes provide discovery, NAT traversal, and
  encrypted forwarding roles when eligible.
- Use rust-libp2p 0.56 for the embedded peer network unless a documented
  milestone explicitly changes that decision.
- SSH may administer test machines, but DLLM peer traffic must not use SSH or
  require SSH configuration.
- Discovery records publish reachability only. They do not grant membership,
  inference access, forwarding eligibility, or any other authorization.
- Owner-signed DLLM state is the authority for membership, policy, transport
  identity bindings, rotation, and revocation.
- Keep DLLM node identity keys separate from libp2p transport identity keys.
- Every node self-binds as owner of its own provisional single-node network at
  bootstrap. `dllm onboard <authority-url>` (or `DLLMD_JOIN_URL`) transitions it
  through `OnboardingStatus`: `Inactive` -> `Joining` -> `Active`/`Failed`.
- While `OnboardingStatus` is `Joining`, `require_active_mode` middleware blocks
  nearly all API routes except `/health`, `/v1/onboarding/status`, and
  `/v1/onboarding/start`.
- `dllmd` runs a hardware auto-benchmark at startup
  (`crates/dllm-daemon/src/hardware_benchmark.rs`) to determine achievable
  `gpu_layers`, then merges the result into the node's hardware profile.
  `DLLMD_GPU_LAYERS` overrides it explicitly.

## Workspace crates

```
crates/dllm-protocol     — Shared types: network state, membership, identity,
                            policy, signed tokens, transport identity bindings
crates/dllm-daemon       — Node daemon: API server, credentials, inference
                            registry, network store (binary name: dllmd)
crates/dllm-cli          — CLI client (binary name: dllm)
crates/dllm-runtime      — Inference runtime: manages llama.cpp child processes
                            (bundled dllm-llama-server, or an external
                            llama-server-compatible binary)
crates/dllm-transport    — libp2p peer transport layer
crates/dllm-probe        — Standalone libp2p diagnostic tool (binary name:
                            dllm-dev-probe)
crates/dllm-llama-server — Bundled OpenAI-compatible llama.cpp server,
                            vendored from llama-cpp-rs (binary name:
                            dllm-llama-server)
```

## Key directories

```
.github/     — CI workflows
docs/        — Phase plans and milestone evidence
docker/      — Dockerfiles for dllmd and CUDA runtime container builds
manifests/   — Model manifest files (GGUF quantization specs)
apps/web/    — Web UI
```

## Task runner

Building requires CMake, a C++ compiler, and `libclang` (for bindgen), since
`dllm-llama-server` compiles llama.cpp from source. On Debian/Ubuntu:
`sudo apt-get install -y cmake build-essential libclang-dev`. `mise install`
does not provision these.

Use [Mise](https://mise.jdx.dev/) to run project-wide tasks. After installing Mise, run:

```sh
mise install
```

Available tasks:

```sh
mise run fmt         # Format Rust (apps/web has no fmt script configured yet)
mise run lint        # Run clippy and web type checks
mise run test        # Run Rust tests and web type checks
mise run build       # Build Rust release binaries and the web app
mise run dev:web     # Start the web app development server
mise run dev:daemon  # Run the dllmd daemon
mise run dev:cli     # Run the dllm CLI
mise run dev:gpu-nodes         # Start the owner GPU dev node (docker compose, GPU 0)
mise run dev:gpu-image:build   # Build the CUDA dev image locally
mise run dev:gpu-nodes:local   # Start the GPU dev node from the local CUDA image
mise run dev:gpu-nodes:down    # Stop the GPU dev nodes
mise run ci          # Run fmt, lint, test, and build
```

Use these tasks as the default entry point for builds and CI checks.

## Validation

Run these checks before completing a milestone. Alternatively, run `mise run ci`.

```sh
cargo fmt --all
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check
```

Build and run:

```sh
cargo build --release
cargo run --release --bin dllmd -- --help
cargo run --release --bin dllm -- --help
```
