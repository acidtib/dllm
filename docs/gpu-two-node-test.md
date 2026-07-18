# GPU two-node test (two 1080s, one host)

Manual test plan for two GPU-pinned `dllmd` nodes on one machine, each bound
to its own NVIDIA GPU via Docker. This mirrors `docs/cpu-two-node-test.md`'s
join flow without needing a second physical machine.

Prerequisites: Docker with the NVIDIA Container Toolkit configured, two
NVIDIA GPUs, access to the repository's GitHub Container Registry package,
and a native `dllm` CLI build for running management commands against the
containers:

```sh
cargo build --release --bin dllm
```

`dllm` does not link against `llama-cpp-4`, so this needs no GPU toolchain
even though the daemons it talks to are GPU-backed.

## 1. Start node-a (owner)

```sh
mise run dev:gpu-nodes
```

This creates `.dev-gpu/node-a/` and `.dev-gpu/node-b/` as the host user, pulls
`ghcr.io/acidtib/dllm:cuda`, and starts node-a detached on GPU 0.
`.dev-gpu/node-a/` holds its state and keys. On first boot it downloads the
model, creates its identities, and self-binds as owner of a new network.

The `Docker images` GitHub Actions workflow is manually triggered from the
Actions tab. It builds and publishes `:cpu`, `:cuda`, and `:vulkan` tags, plus
immutable tags containing the source commit SHA. To test a specific build or
another registry, set `DLLM_DEV_GPU_IMAGE` before running the Mise task:

```sh
DLLM_DEV_GPU_IMAGE=ghcr.io/acidtib/dllm:cuda-<commit-sha> \
  mise run dev:gpu-nodes
```

For an intentional local CUDA build, run `mise run dev:gpu-image:build`, then
`mise run dev:gpu-nodes:local`.

Give it time to download and load the model, then sanity-check it:

```sh
./target/release/dllm --daemon http://127.0.0.1:7337 \
  --management-token node-a-token assign my-model --owner

curl -s http://127.0.0.1:7337/v1/chat/completions \
  -H "Authorization: Bearer node-a-key" \
  -H "Content-Type: application/json" \
  -d '{"model":"my-model","messages":[{"role":"user","content":"Say hi in 5 words."}],"stream":false}' | jq .
```

## 2. Prepare node-b's identity

Node-b must not start before it has approved network state. Otherwise, it
would bootstrap a separate provisional network.

```sh
./target/release/dllm \
  --node-key .dev-gpu/node-b/node.key \
  --transport-key .dev-gpu/node-b/transport.key \
  init
```

The command creates both identities and prints node-b's transport peer ID.
Copy the peer ID for step 3.

```sh
./target/release/dllm --node-key .dev-gpu/node-b/node.key \
  request-access http://127.0.0.1:7337
```

Both nodes are on the same host, so this talks directly to node-a's
management API on loopback. No SSH tunnel is needed.

## 3. Approve on node-a

```sh
./target/release/dllm --daemon http://127.0.0.1:7337 \
  --management-token node-a-token list-access-requests

./target/release/dllm --daemon http://127.0.0.1:7337 \
  --management-token node-a-token \
  approve-access .dev-gpu/node-b/node.key --endpoint http://127.0.0.1:7338

./target/release/dllm --daemon http://127.0.0.1:7337 \
  --management-token node-a-token \
  bind-transport <peer-id-from-step-2> --binding-generation 1 \
  --expires-at-unix 2000000000
```

Both nodes' state lives on the same filesystem, so copy the signed state
directly:

```sh
cp .dev-gpu/node-a/state.json .dev-gpu/node-b/state.json
```

## 4. Start node-b

```sh
docker compose -f docker-compose.dev-gpu.yml up -d node-b
```

Node-b starts on GPU 1. It loads the approved state copied in step 3 and
dials node-a at `/ip4/127.0.0.1/tcp/7444`. It does not need model settings
because it is not hosting inference.

## 5. Verify

```sh
./target/release/dllm --daemon http://127.0.0.1:7338 \
  --management-token node-b-token peer-status

./target/release/dllm --daemon http://127.0.0.1:7337 \
  --management-token node-a-token status
```

Node-a's member count should include node-b, and `peer-status` should show a
connected peer.

Confirm that each container sees only its assigned GPU:

```sh
docker compose -f docker-compose.dev-gpu.yml exec node-a nvidia-smi
docker compose -f docker-compose.dev-gpu.yml exec node-b nvidia-smi
```

## Tear down

```sh
mise run dev:gpu-nodes:down
```

Delete `.dev-gpu/` to reset both nodes' state entirely.
