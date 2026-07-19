# CPU-only two-node test (host + laptop)

Manual test plan for the new `~/.dllm` default paths: run `dllmd` on the host
with a small CPU model, then add a laptop as a second peer node.

**Model:** `Qwen/Qwen2.5-0.5B-Instruct-GGUF` — small (~350MB at the
auto-picked Q4_K_M quant), fast enough on CPU to iterate with.

**Why SSH shows up here:** `dllmd` refuses to bind its HTTP API to a
non-loopback address unless `DLLMD_TLS_CERT`/`DLLMD_TLS_KEY` are also set
(`crates/dllm-daemon/src/main.rs:82-89`), and the `dllm` CLI has no way to
skip cert validation. Simplest path: leave the owner's HTTP API on loopback
and SSH-tunnel to it just for the one-time admin commands below. That's
administering a test machine, not DLLM peer traffic, so it doesn't conflict
with this repo's "no SSH for DLLM traffic" rule. The P2P mesh itself (port
7444) always binds `0.0.0.0` regardless and talks directly over the LAN with
its own encryption, untouched by any of this.

## 1. Host (owner)

```sh
cd /home/acidtib/Code/dllm
cargo build --release   # skip if binaries are already current

DLLMD_NETWORK=my-network \
DLLMD_MANAGEMENT_TOKEN=my-secret-token \
DLLMD_API_KEY=my-api-key \
DLLMD_HF_MODEL=Qwen/Qwen2.5-0.5B-Instruct-GGUF \
DLLMD_GPU_LAYERS=0 \
DLLMD_P2P_ENABLED=true \
./target/release/dllmd
```

`DLLMD_GPU_LAYERS=0` here is an explicit override for this CPU-only test.
Omitting `DLLMD_GPU_LAYERS`/`DLLMD_CONTEXT_SIZE` lets `dllmd` auto-fit both
values against detected device memory instead of using the hardcoded
`38`/`2048` defaults; see `docs/dllm-proposal.md` section 9.3.

This downloads the model, creates `~/.dllm/{state.json,owner.key,transport.key}`,
self-binds the owner's own transport identity as part of that same
bootstrap, and starts listening on `0.0.0.0:7444` for P2P -- one boot, no
manual `init-transport`/`bind-transport` step needed for the owner's own
node. Give it a minute for the model download. In another terminal,
sanity-check it works:

```sh
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token assign my-model --owner

curl -s http://127.0.0.1:7337/v1/chat/completions \
  -H "Authorization: Bearer my-api-key" -H "Content-Type: application/json" \
  -d '{"model":"my-model","messages":[{"role":"user","content":"Say hi in 5 words."}],"stream":false}' | jq .
```

## 2. Laptop

```sh
cargo build --release   # or copy the binaries if same OS/arch
./target/release/dllm init             # creates ~/.dllm/node.key
./target/release/dllm init-transport   # creates ~/.dllm/transport.key, prints a peer ID -- copy it
```

Get the host's LAN IP on the host (`hostname -I` or `ip -4 addr show`) --
call it `HOST_IP`. Tunnel to the owner's admin API:

```sh
ssh -f -N -L 7337:127.0.0.1:7337 you@HOST_IP
./target/release/dllm request-access http://127.0.0.1:7337
```

## 3. Host -- approve

`approve-access` needs a local copy of the laptop's *private* node key to
derive its pubkey (just how the command works today):

```sh
scp laptop-user@laptop-ip:~/.dllm/node.key /tmp/laptop-node.key

dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token list-access-requests
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  approve-access /tmp/laptop-node.key --endpoint http://laptop-ip:7337
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token \
  bind-transport <peer-id-from-init-transport> --binding-generation 1 --expires-at-unix 2000000000

rm /tmp/laptop-node.key
scp ~/.dllm/state.json laptop-user@laptop-ip:~/.dllm/state.json
```

## 4. Laptop -- join the mesh

```sh
DLLMD_P2P_ENABLED=true \
DLLMD_P2P_BOOTSTRAP=/ip4/HOST_IP/tcp/7444 \
DLLMD_MANAGEMENT_TOKEN=laptop-token \
./target/release/dllmd
```

`DLLMD_STATE`, `DLLMD_NODE_KEY`, `DLLMD_P2P_KEY` all default to `~/.dllm/*`
now (that's the thing being tested), so no explicit paths are needed. No
`owner.key` is present, so it loads as a replica. No model env vars are
needed -- it doesn't have to host inference.

## 5. Verify

```sh
# laptop
dllm --daemon http://127.0.0.1:7337 --management-token laptop-token peer-status
# host
dllm --daemon http://127.0.0.1:7337 --management-token my-secret-token status
```

Host's member count should include the laptop; `peer-status` should show a
connected peer.
