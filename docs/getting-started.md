# Getting started with DLLM

This guide walks through setting up DLLM on a single machine, adding a second
node (for example a laptop), and sending your first chat completion request.

## Prerequisites

- A machine with Docker, or Rust 1.96+ if building from source. Building from
  source also requires CMake and a C++ compiler, since `dllmd` bundles and
  compiles its own inference runtime (`dllm-llama-server`) from vendored
  llama.cpp source.
- A GGUF-format model file, or a Hugging Face repo id if you want `dllmd` to
  download one for you.
- Two machines if you want to test multi-node setup (for example a desktop and a
  laptop on the same LAN).

## Quick start with Docker

Published images are available from GitHub Container Registry after running
the manually triggered `Docker images` workflow:

```sh
docker pull ghcr.io/acidtib/dllm:cpu
docker pull ghcr.io/acidtib/dllm:cuda-sm61
docker pull ghcr.io/acidtib/dllm:vulkan
```

CUDA images are architecture-specific. The suffix is the NVIDIA compute
capability, such as `sm61` for Pascal or `sm86` for Ampere. The `CUDA runtime
images` workflow publishes one image per selected architecture. It compiles
the expensive CUDA runtime separately so routine DLLM image builds can reuse
it. The workflows also publish tags containing the source commit SHA. Use
those when a deployment must remain pinned to one source revision.

Available CUDA tags are `cuda-sm61`, `cuda-sm70`, `cuda-sm75`, `cuda-sm80`,
`cuda-sm86`, `cuda-sm89`, `cuda-sm90`, `cuda-sm100`, and `cuda-sm120`. Select
the tag matching the GPU's compute capability. The unqualified `cuda` tag
tracks the highest supported architecture, currently `cuda-sm120`, and may
not run on older GPUs. Increment the workflow's runtime version input when
llama.cpp, its Rust wrapper, CUDA, NCCL, or the runtime build configuration
changes. Routine DLLM source changes do not need a new CUDA runtime version.

To build locally instead, run from the repository root.
`docker/Dockerfile.dllmd` has
three targets sharing one build: `runtime-cpu` (no GPU), `runtime-cuda`
(NVIDIA), and `runtime-vulkan` (AMD/Intel/NVIDIA via Vulkan).

```sh
docker build -f docker/Dockerfile.dllmd --target runtime-cpu -t dllm .
```

For GPU-backed inference, build `runtime-cuda` or `runtime-vulkan` instead.
See `docs/gpu-two-node-test.md` for a full GPU dev-node walkthrough:

```sh
docker build -f docker/Dockerfile.cuda-runtime \
  --build-arg CUDA_ARCHITECTURES=61 -t dllm:cuda-runtime-sm61-v1 .
docker build -f docker/Dockerfile.dllmd --target runtime-cuda \
  --build-arg CUDA_RUNTIME_IMAGE=dllm:cuda-runtime-sm61-v1 \
  -t dllm:cuda-sm61 .
```

The CUDA runtime defaults to compute capability `61`, matching the GTX 1080
development host and avoiding an expensive all-architectures llama.cpp build.
Set `CUDA_ARCHITECTURES` on the runtime build for other hardware, for example:

```sh
docker build -f docker/Dockerfile.cuda-runtime \
  --build-arg CUDA_ARCHITECTURES=86 -t dllm:cuda-runtime-sm86-v1 .
docker build -f docker/Dockerfile.dllmd --target runtime-cuda \
  --build-arg CUDA_RUNTIME_IMAGE=dllm:cuda-runtime-sm86-v1 \
  -t dllm:cuda-sm86 .
```

`dllmd` bundles its own inference runtime (`dllm-llama-server`, built from
vendored llama.cpp source) and starts it automatically once you point it at a
model, either a mounted GGUF file or a Hugging Face repo id. A model is still
supplied separately; the image does not include one. `DLLMD_RUNTIME_URL` and
`DLLMD_RUNTIME_BIN` remain available if you would rather point `dllmd` at an
external runtime instead.

### Run the daemon

Create a directory for persistent state and keys:

```sh
mkdir -p ~/.dllm
```

Start the daemon with a network name, a management token, and an API key so
clients can make inference requests:

```sh
docker run --rm -it \
  --network host \
  -v ~/.dllm:/var/lib/dllm \
  -e DLLMD_NETWORK=my-network \
  -e DLLMD_MANAGEMENT_TOKEN=my-secret-token \
  -e DLLMD_API_KEY=my-api-key \
  dllm
```

On first boot, since `~/.dllm` has no `state.json` yet, `dllmd` creates separate
node, authority, and transport identities plus a signed provisional network
state named `my-network`. There is
no separate network-creation step. `DLLMD_NETWORK` only matters on that first
boot; once `state.json` exists, the network's name is fixed and later
restarts just load it (`DLLMD_NETWORK` is ignored at that point).

The daemon starts on `http://127.0.0.1:7337`. It exposes two ports:
- `7337` (TCP): HTTP management and inference API.
- `7444` (TCP+UDP): libp2p peer transport (when P2P is enabled).

> **Do not run `dllm create` against the same state/owner-key files while
> `dllmd` is already running against them.** `dllm create` writes directly to
> disk and the daemon never re-reads its state file after startup, so a
> `create` run after the daemon has booted will be silently overwritten the
> next time the daemon persists any change.

### Check status

```sh
docker run --rm --network host \
  -v ~/.dllm:/var/lib/dllm \
  --entrypoint dllm \
  dllm \
  --state /var/lib/dllm/state.json \
  --authority-key /var/lib/dllm/authority.key \
  --daemon http://127.0.0.1:7337 \
  --management-token my-secret-token \
  status
```

Output:
```
network my-network generation 1 members 1
forwarding 0  bindings 0 (0 expire <24h)  budgets 0  bans 0
```

## Building from source (no Docker)

```sh
cargo build --release
```

The binaries are at `target/release/dllmd` and `target/release/dllm`.

By default, both `dllmd` and `dllm` store their state and keys under
`~/.dllm/` (`state.json`, `authority.key`, `node.key`, `transport.key`).
Override individual paths with `DLLMD_STATE`, `DLLMD_AUTHORITY_KEY`,
`DLLMD_NODE_KEY`, `DLLMD_P2P_KEY` (`dllmd`) or `--state`, `--authority-key`,
`--node-key`, `--transport-key` (`dllm`).

If `DLLMD_MANAGEMENT_TOKEN` or `DLLMD_API_KEY` aren't set, `dllmd`
generates them on first boot, prints them once, and persists them to
`~/.dllm/config.json` (mode `0600`). `dllm` reads the same file as a
fallback for `--management-token`, so once `dllmd` has booted at least
once on a machine, `dllm status` and friends work with no flags.

## Run inference locally

`dllmd` bundles its own inference runtime (`dllm-llama-server`, compiled from
vendored llama.cpp source) and resolves it as a sibling of the `dllmd`
executable. `cargo build --release --workspace` builds both, so there is no
separate `llama-server` install step. Point `dllmd` at a model and it starts
the runtime for you.

When `DLLMD_HF_MODEL` is used, downloads are cached under `~/.dllm/models`
by default. Set `HF_HOME` yourself to use a different (or already
populated) Hugging Face cache instead.

Use a local GGUF file with `DLLMD_MODEL_PATH`:

```sh
DLLMD_MODEL_PATH=~/models/qwen2.5-7b-instruct-q4_k_m.gguf ./target/release/dllmd
```

Or let `dllmd` download a model from Hugging Face with `DLLMD_HF_MODEL`:

```sh
DLLMD_HF_MODEL=Qwen/Qwen2.5-7B-Instruct-GGUF ./target/release/dllmd
```

`DLLMD_MODEL_PATH` and `DLLMD_HF_MODEL` are mutually exclusive. Both paths
accept `DLLMD_RUNTIME_PORT`, `DLLMD_GPU_LAYERS`, and `DLLMD_CONTEXT_SIZE` to
tune the runtime, and `DLLMD_MMPROJ_PATH` to enable multimodal input.

### Using an external runtime instead

If you would rather manage a `llama-server`-compatible binary yourself, set
`DLLMD_RUNTIME_BIN` alongside `DLLMD_MODEL_PATH` and `dllmd` will start that
binary instead of the bundled one:

```sh
DLLMD_RUNTIME_BIN=/usr/bin/llama-server \
DLLMD_MODEL_PATH=~/models/qwen2.5-7b-instruct-q4_k_m.gguf \
DLLMD_RUNTIME_PORT=8081 \
DLLMD_GPU_LAYERS=38 \
DLLMD_CONTEXT_SIZE=2048 \
./target/release/dllmd
```

If you already have a runtime running elsewhere, point `dllmd` at it directly:

```sh
DLLMD_RUNTIME_URL=http://127.0.0.1:8081 ./target/release/dllmd
```

## Assign a model

With the daemon running and a runtime available, assign a model to a node:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  assign my-model --owner
```

The authority node will be assigned the model. The daemon resolves the runtime
placement automatically.

## Send a chat completion request

The daemon exposes an OpenAI-compatible endpoint at `/v1/chat/completions`.
Use an API key set via `DLLMD_API_KEY`:

```sh
curl -s http://127.0.0.1:7337/v1/chat/completions \
  -H "Authorization: Bearer my-api-key" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "my-model",
    "messages": [{"role": "user", "content": "What is the capital of France?"}],
    "stream": false
  }' | jq .
```

For streaming responses, set `"stream": true`. The response arrives as
Server-Sent Events (SSE).

## Adding a second node

The authority daemon must be reachable over HTTPS and must advertise a dialable
libp2p address. For example:

```sh
DLLMD_P2P_ADVERTISE=/ip4/192.168.1.10/tcp/7444 dllmd
```

On the joining machine, start the daemon with the authority URL:

```sh
DLLMD_JOIN_URL=https://authority.example:7337 dllmd
```

Alternatively, start `dllmd` normally and direct the running local daemon to
join later:

```sh
dllmd
dllm onboard https://authority.example:7337
```

Keep the joining daemon running while `dllm onboard` waits. Both entry paths
use the same daemon-owned workflow and preserve existing node and transport
identities.

On the authority machine, list and approve the request:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  list-access-requests
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  approve-access <node-public-key-hex>
```

Approval adds membership and the submitted transport identity atomically. The
joining daemon fetches and verifies the signed state, persists the advertised
bootstrap addresses, and activates without a restart. Verify it locally:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token laptop-token peer-status
```

Output includes the peer ID, discovery mode, DHT hosting role, forwarding
eligibility, current path (direct or forwarded), and stream counters.

On the authority node:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token status
```

The member count now includes the joining node.

Manual `dllm init`, `dllm request-access`, state copying, explicit bootstrap
configuration, and `dllm bind-transport` remain available for troubleshooting
and advanced recovery.

## Using the Web UI

Open `apps/web/index.html` in a browser, or serve it from the daemon if you have
configured a static file path. Enter your management token and click Refresh.
The UI shows network state, nodes, model placements, peer diagnostics, access
requests, budgets, moderation tools, and the audit log.

## Environment variable reference

| Variable | Default | Purpose |
|---|---|---|
| `DLLMD_BIND` | `127.0.0.1:7337` | HTTP listen address |
| `DLLMD_STATE` | `~/.dllm/state.json` | Path to signed network state |
| `DLLMD_NETWORK` | generated (`dllm-<word>-<word>`) | Name for the network `dllmd` bootstraps on first boot (only used when `DLLMD_STATE` does not yet exist) |
| `DLLMD_AUTHORITY_KEY` | `~/.dllm/authority.key` | Authority Ed25519 private key (32 bytes) |
| `DLLMD_JOIN_URL` | none | HTTPS authority URL for automatic onboarding |
| `DLLMD_NODE_KEY` | `~/.dllm/node.key` | Local node Ed25519 private key |
| `DLLMD_MANAGEMENT_TOKEN` | generated, see `~/.dllm/config.json` | Bearer token for management API |
| `DLLMD_API_KEY` | generated, see `~/.dllm/config.json` | Bearer token for inference API |
| `DLLMD_RUNTIME_URL` | none | URL of an external, separately managed runtime server |
| `DLLMD_RUNTIME_BIN` | none | Path to an external `llama-server`-compatible binary (auto-started instead of the bundled runtime) |
| `DLLMD_MODEL_PATH` | none | Path to a local GGUF model file, used by the bundled runtime, or by `DLLMD_RUNTIME_BIN` if set |
| `DLLMD_HF_MODEL` | none | Hugging Face repo id to download and serve via the bundled runtime |
| `DLLMD_MMPROJ_PATH` | none | Path to a multimodal projector file for the bundled runtime (optional) |
| `DLLMD_RUNTIME_PORT` | `8081` | Port for the auto-started runtime (bundled or external) |
| `DLLMD_GPU_LAYERS` | `38` | Layers to offload to GPU (bundled or external runtime) |
| `DLLMD_CONTEXT_SIZE` | `2048` | Context window size (bundled or external runtime) |
| `DLLMD_P2P_ENABLED` | `true` | Enable embedded peer transport |
| `DLLMD_P2P_PORT` | `7444` | libp2p listen port |
| `DLLMD_P2P_KEY` | `~/.dllm/transport.key` | libp2p Ed25519 identity |
| `DLLMD_P2P_BOOTSTRAP` | none | Comma-separated bootstrap multiaddrs |
| `DLLMD_P2P_ADVERTISE` | none | Comma-separated dialable authority multiaddrs returned during onboarding |
| `DLLMD_P2P_DISCOVERY_MODE` | `listed` | `listed` or `unlisted` |
| `DLLMD_P2P_DHT_HOSTING` | `true` | `true` = DHT server, `false` = client only |
| `DLLMD_ACCESS_REQUEST_RATE_LIMIT` | `10` | Max access requests per window |
| `DLLMD_ACCESS_REQUEST_RATE_WINDOW` | `60` | Rate-limit window in seconds |
| `DLLMD_P2P_MAX_ESTABLISHED_INCOMING` | none | libp2p inbound connection cap |
| `DLLMD_P2P_MAX_ESTABLISHED_PER_PEER` | none | Per-peer connection cap |
| `DLLMD_P2P_MAX_PENDING_INCOMING` | none | Pending connection cap |
| `DLLMD_ADMISSION_LIMIT` | `1` | Max concurrent inference requests |
| `DLLMD_TLS_CERT` | none | TLS certificate path (PEM) |
| `DLLMD_TLS_KEY` | none | TLS private key path (PEM) |
| `DLLMD_PUBLIC_URL` | `http://{bind}` | Public URL for replica endpoints |
