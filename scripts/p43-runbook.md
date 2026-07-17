# P4.3 Physical Validation Runbook

## Prerequisites

- Release binary built: `cargo build --release --bin dllmd --bin dllm`
- Fixture runtime: `scripts/fixture-runtime.py` (Python 3, no deps)
- Three machines: Kansas VPS, New York VPS, and one edge (laptop or local)
- The local dev box acts as the owner and test client

## Topology

```
Owner (local dev box)
  |  creates signed state, bindings, membership, assignments
  |  also acts as test client for curl requests

Kansas (165.245.194.225) — bootstrap + forwarding member + fixture runtime
New York (147.182.129.159) — forwarding member + fixture runtime
Laptop (192.168.1.189) — edge node behind NAT (reserves forwarded path)
```

Kansas and New York run the fixture runtime on 127.0.0.1:8081.
The owner assigns the model to New York so inference routes there.

## Phase 1: Deploy

On each remote host, copy the binary and fixture:
```
scp target/release/dllmd scripts/fixture-runtime.py root@165.245.194.225:~/
scp target/release/dllmd scripts/fixture-runtime.py root@147.182.129.159:~/
scp target/release/dllmd scripts/fixture-runtime.py acidito@192.168.1.189:~/
```

## Phase 2: Generate keys and state (local dev box)

```bash
# Create owner network
export DLLMD_BIND=127.0.0.1:7339
export DLLMD_STATE=./p43-owner-state.json
export DLLMD_OWNER_KEY=./p43-owner.key
export DLLMD_NETWORK=p43-test
export DLLMD_P2P_ENABLED=true
export DLLMD_P2P_PORT=7445
export DLLMD_P2P_KEY=./p43-owner-transport.key

cargo run --release --bin dllmd &
OWNER_PID=$!
sleep 2

# Get owner info
OWNER_PUBKEY=$(curl -s http://127.0.0.1:7339/v1/status | jq -r '.network.state.owner_pubkey')
OWNER_PEER=$(curl -s http://127.0.0.1:7339/v1/peer-network/status | jq -r '.peer_id')
echo "Owner pubkey: $OWNER_PUBKEY"
echo "Owner PeerId: $OWNER_PEER"

kill $OWNER_PID
```

## Phase 3: Start nodes

On each remote host, start the fixture runtime first, then dllmd.

### Kansas (bootstrap + forwarding)
```bash
python3 ~/fixture-runtime.py 8081 &
DLLMD_BIND=0.0.0.0:7337 \
DLLMD_STATE=~/p43-state.json \
DLLMD_OWNER_KEY=~/p43-owner.key \
DLLMD_NETWORK=p43-test \
DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
DLLMD_ADMISSION_LIMIT=4 \
DLLMD_P2P_ENABLED=true \
DLLMD_P2P_PORT=7444 \
DLLMD_P2P_KEY=~/p43-transport.key \
./dllmd &
```

### New York (forwarding member, model target)
```bash
python3 ~/fixture-runtime.py 8081 &
# Copy state from Kansas first (scp root@165.245.194.225:~/p43-state.json ~/)
DLLMD_BIND=0.0.0.0:7337 \
DLLMD_STATE=~/p43-state.json \
DLLMD_NETWORK=p43-test \
DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
DLLMD_ADMISSION_LIMIT=4 \
DLLMD_P2P_ENABLED=true \
DLLMD_P2P_PORT=7444 \
DLLMD_P2P_KEY=~/p43-transport.key \
DLLMD_P2P_BOOTSTRAP="/ip4/165.245.194.225/tcp/7444" \
./dllmd &
```

### Laptop (edge)
```bash
DLLMD_BIND=0.0.0.0:7337 \
DLLMD_STATE=~/p43-state.json \
DLLMD_NETWORK=p43-test \
DLLMD_ADMISSION_LIMIT=2 \
DLLMD_P2P_ENABLED=true \
DLLMD_P2P_PORT=7444 \
DLLMD_P2P_KEY=~/p43-transport.key \
DLLMD_P2P_BOOTSTRAP="/ip4/165.245.194.225/tcp/7444" \
DLLMD_P2P_RESERVE=true \
./dllmd &
```

## Phase 4: Create bindings, members, assignments

On the local dev box (owner):
```bash
# Get each node's PeerId and bind transport
NY_PEER=$(curl -s http://147.182.129.159:7337/v1/peer-network/status | jq -r '.peer_id')
KS_PEER=$(curl -s http://165.245.194.225:7337/v1/peer-network/status | jq -r '.peer_id')
LP_PEER=$(curl -s http://192.168.1.189:7337/v1/peer-network/status | jq -r '.peer_id')

# Generate node keys on each machine, or use fixed ones for testing
# For the fixture, use deterministic keys:
NY_KEY="0000000000000000000000000000000000000000000000000000000000000001"
KS_KEY="0000000000000000000000000000000000000000000000000000000000000002"
LP_KEY="0000000000000000000000000000000000000000000000000000000000000003"
OWNER_KEY_HEX="..."

# Join members (from the owner node)
curl -s -X POST http://127.0.0.1:7339/v1/members/join \
  -H "Content-Type: application/json" \
  -d "{\"token\":$(dllm invite),\"node_pubkey\":\"$NY_KEY\",\"node_endpoint\":\"http://147.182.129.159:7337\"}"

# ...repeat for KS and LP...

# Bind transports
dllm bind-transport $NY_KEY $NY_PEER 1 2000000000
dllm bind-transport $KS_KEY $KS_PEER 1 2000000000
dllm bind-transport $LP_KEY $LP_PEER 1 2000000000

# Set forwarding policy
dllm set-forwarder $NY_KEY 4
dllm set-forwarder $KS_KEY 4

# Assign model to New York
dllm assign test-model $NY_KEY

# Distribute state to members
scp ./p43-owner-state.json root@165.245.194.225:~/p43-state.json
scp ./p43-owner-state.json root@147.182.129.159:~/p43-state.json
scp ./p43-owner-state.json acidito@192.168.1.189:~/p43-state.json
```

## Phase 5: Run scenarios

### 1. Direct path (local → Kansas)
```bash
curl -s http://165.245.194.225:7337/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"test-model","stream":true}'
# Expected: SSE chunks, 200 OK
# Record: timing, response length, path from peer-network/status
```

### 2. Forwarded path
On New York, block UDP to Kansas:
```bash
ssh root@147.182.129.159 "iptables -A OUTPUT -d 165.245.194.225 -p udp --dport 7444 -j DROP"
```
Request from laptop:
```bash
curl -s http://147.182.129.159:7337/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"test-model","stream":true}'
# Should route through Kansas as relay
# Record: path="forwarded", selected_forwarder
# Clean up: ssh root@147.182.129.159 "iptables -D OUTPUT -d 165.245.194.225 -p udp --dport 7444 -j DROP"
```

### 3. Concurrency ceiling (admission_limit=4)
```bash
for i in $(seq 1 6); do
  curl -s -o /dev/null -w "%{http_code}\n" \
    http://147.182.129.159:7337/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d '{"model":"test-model"}' &
done
wait
# Expected: 4x 200, 2x 429 (or error frame for libp2p path)
```

### 4. Cancellation
```bash
# Slow request (20 chunks, 500ms each = 10 seconds total)
curl -s --max-time 1 \
  http://147.182.129.159:7337/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Chunk-Count: 20" \
  -H "X-Delay-Ms: 500" \
  -d '{"model":"test-model","stream":true}'
# Expected: curl exits after 1s, serving node cancels runtime request
# Check: GET /v1/peer-network/status → cancelled_streams > 0
```

### 5. Deadline
```bash
# Request with 30s chunks but 2s deadline via proxy
# The proxy_peer function sends deadline_ms = now_ms + 60000 by default.
# For a tighter test, use the HTTP path with a short client timeout.
curl -s --max-time 3 \
  http://147.182.129.159:7337/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Chunk-Count: 100" \
  -H "X-Delay-Ms: 1000" \
  -d '{"model":"test-model","stream":true}'
# Expected: deadline error in stream or truncated response
# Check: GET /metrics → deadline_expirations
```

### 6. Live authorization
```bash
# Start a slow request in background
curl -s http://147.182.129.159:7337/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "X-Chunk-Count: 30" \
  -H "X-Delay-Ms: 1000" \
  -d '{"model":"test-model","stream":true}' > /tmp/stream-out.txt &
STREAM_PID=$!
sleep 2

# Revoke the transport binding
dllm revoke-transport $NY_KEY

# Distribute new state
scp ./p43-owner-state.json root@147.182.129.159:~/p43-state.json
# Note: dllmd must reload state (currently requires restart or SIGHUP)
# For now, restart the daemon on New York to pick up the new state

# Check: active stream should be terminated
# New request from the same peer should be rejected
```

### 7. Recovery
```bash
# Kill the forwarding node
ssh root@165.245.194.225 "kill \$(pgrep dllmd)"

# Wait for reselection
sleep 10

# Check edge node status
curl -s http://192.168.1.189:7337/v1/peer-network/status | jq '{selected_forwarder, reselections}'
# Expected: reselections incremented, new forwarder = New York

# Request should still work
curl -s http://192.168.1.189:7337/v1/peer-network/status
```

### 8. Restart / address change
```bash
# Restart New York daemon
ssh root@147.182.129.159 "kill \$(pgrep dllmd) && sleep 2"
ssh root@147.182.129.159 "DLLMD_BIND=0.0.0.0:7337 DLLMD_STATE=~/p43-state.json ... ./dllmd &"
sleep 10

# Request from edge should succeed via Kademlia-learned address
curl -s http://192.168.1.189:7337/v1/peer-network/status
```

### 9. Security observation
```bash
# On each remote host:
ss -tlnp | grep -E '7337|7444|8081'
# 8081 (runtime) must be on 127.0.0.1 only
# 7337 (API) and 7444 (P2P) can be on 0.0.0.0

# No SSH peer traffic
ss -tnp | grep ':22'  # only the admin SSH session

# No owner key on members
ssh root@147.182.129.159 "ls ~/p43-owner.key 2>&1"
# Expected: No such file

# No separate relay binary
ssh root@165.245.194.225 "ls ~/dllm-relay ~/dllm-tunnel 2>&1"
# Expected: No such file
```

## Phase 6: Collect evidence

Record these values:
- Commit hash: `git rev-parse HEAD`
- Binary size: `ls -lh target/release/dllmd`
- Test timings: each scenario's latency
- Diagnostics: `GET /v1/peer-network/status` output after each scenario
- Metrics: `GET /metrics` output after load test
- Topology: which node played which role

## Phase 7: Cleanup

On each host:
```bash
kill $(pgrep dllmd) 2>/dev/null || true
kill $(pgrep -f fixture-runtime) 2>/dev/null || true
rm -f ~/dllmd ~/fixture-runtime.py ~/p43-*.json ~/p43-*.key
iptables -F 2>/dev/null || true
```

Verify ports closed:
```bash
ss -tlnp | grep -E '7337|7444|8081'
# Expected: empty
```
