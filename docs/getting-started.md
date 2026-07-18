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

Build the image from the repository root:

```sh
docker build -t dllm .
```

`dllmd` bundles its own inference runtime (`dllm-llama-server`, built from
vendored llama.cpp source) and starts it automatically once you point it at a
model, either a mounted GGUF file or a Hugging Face repo id. A model is still
supplied separately; the image does not include one. `DLLMD_RUNTIME_URL` and
`DLLMD_RUNTIME_BIN` remain available if you would rather point `dllmd` at an
external runtime instead. Note that whether a given Docker build of this image
includes the compiled bundled runtime binary depends on how that image was
built; check the image you are using if the bundled-runtime path does not
start as expected.

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

On first boot, since `~/.dllm` has no `state.json` yet, `dllmd` creates an
owner identity and a signed network state named `my-network` itself. There is
no separate network-creation step. `DLLMD_NETWORK` only matters on that first
boot; once `state.json` exists, the network's name is fixed and later
restarts just load it (`DLLMD_NETWORK` is ignored at that point).

The daemon starts on `http://127.0.0.1:7337`. It exposes two ports:
- `7337` (TCP) — HTTP management and inference API.
- `7444` (TCP+UDP) — libp2p peer transport (when P2P is enabled).

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
  --owner-key /var/lib/dllm/owner.key \
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
`~/.dllm/` (`state.json`, `owner.key`, `node.key`, `transport.key`).
Override individual paths with `DLLMD_STATE`, `DLLMD_OWNER_KEY`,
`DLLMD_NODE_KEY`, `DLLMD_P2P_KEY` (`dllmd`) or `--state`, `--owner-key`,
`--node-key`, `--transport-key` (`dllm`).

## Run inference locally

`dllmd` bundles its own inference runtime (`dllm-llama-server`, compiled from
vendored llama.cpp source) and resolves it as a sibling of the `dllmd`
executable. `cargo build --release --workspace` builds both, so there is no
separate `llama-server` install step. Point `dllmd` at a model and it starts
the runtime for you.

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

The owner node will be assigned the model. The daemon resolves the runtime
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

## Adding a second node (for example a laptop)

Once the owner node is running, you can add another machine to the network.

### Step 1: generate identities on the laptop

On the laptop, build DLLM or copy the binaries. Generate a node identity and
transport identity:

```sh
dllm init
# Creates ~/.dllm/node.key

dllm init-transport
# Prints a libp2p peer ID, creates ~/.dllm/transport.key
# Example output: 12D3KooW...
```

### Step 2: request access

Submit an access request to the owner daemon. The laptop must be able to reach
the owner's HTTP API (use the LAN IP, not loopback):

```sh
dllm request-access http://192.168.1.10:7337
```

Or use the guided onboarding command:

```sh
dllm onboard http://192.168.1.10:7337
```

`dllm onboard` generates keys if they do not exist, submits the access request,
and polls for approval. It prints what to do next.

### Step 3: approve on the owner node

On the owner machine, list pending requests:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  list-access-requests
```

Approve the laptop:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  approve-access /path/to/laptop-node.key --endpoint http://192.168.1.20:7337
```

### Step 4: bind the transport identity

The owner binds the laptop's libp2p peer ID so it can participate in the peer
network:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  bind-transport 12D3KooW... --binding-generation 1 --expires-at-unix 2000000000
```

### Step 5: start the laptop daemon

On the laptop, start `dllmd` pointing at the owner's state as a replica:

```sh
DLLMD_STATE=/path/to/state.json \
DLLMD_NODE_KEY=/path/to/dllm-node.key \
DLLMD_MANAGEMENT_TOKEN=laptop-token \
DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
DLLMD_P2P_ENABLED=true \
DLLMD_P2P_KEY=/path/to/dllm-transport.key \
DLLMD_P2P_BOOTSTRAP=/ip4/192.168.1.10/tcp/7444 \
./dllmd
```

### Step 6: verify

On the laptop:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token laptop-token peer-status
```

Output includes the peer ID, discovery mode, DHT hosting role, forwarding
eligibility, current path (direct or forwarded), and stream counters.

On the owner:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token status
```

The member count now includes the laptop.

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
| `DLLMD_NETWORK` | `private` | Name for the network `dllmd` bootstraps on first boot (only used when `DLLMD_STATE` does not yet exist) |
| `DLLMD_OWNER_KEY` | `~/.dllm/owner.key` | Owner Ed25519 private key (32 bytes) |
| `DLLMD_NODE_KEY` | same as owner key | Local node Ed25519 private key |
| `DLLMD_MANAGEMENT_TOKEN` | none | Bearer token for management API |
| `DLLMD_API_KEY` | none | Bearer token for inference API |
| `DLLMD_RUNTIME_URL` | none | URL of an external, separately managed runtime server |
| `DLLMD_RUNTIME_BIN` | none | Path to an external `llama-server`-compatible binary (auto-started instead of the bundled runtime) |
| `DLLMD_MODEL_PATH` | none | Path to a local GGUF model file, used by the bundled runtime, or by `DLLMD_RUNTIME_BIN` if set |
| `DLLMD_HF_MODEL` | none | Hugging Face repo id to download and serve via the bundled runtime |
| `DLLMD_MMPROJ_PATH` | none | Path to a multimodal projector file for the bundled runtime (optional) |
| `DLLMD_RUNTIME_PORT` | `8081` | Port for the auto-started runtime (bundled or external) |
| `DLLMD_GPU_LAYERS` | `38` | Layers to offload to GPU (bundled or external runtime) |
| `DLLMD_CONTEXT_SIZE` | `2048` | Context window size (bundled or external runtime) |
| `DLLMD_P2P_ENABLED` | `false` | Enable embedded peer transport |
| `DLLMD_P2P_PORT` | `7444` | libp2p listen port |
| `DLLMD_P2P_KEY` | `~/.dllm/transport.key` | libp2p Ed25519 identity |
| `DLLMD_P2P_BOOTSTRAP` | none | Comma-separated bootstrap multiaddrs |
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
