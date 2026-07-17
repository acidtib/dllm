#!/usr/bin/env bash
# P4.3 physical validation orchestration.
# Run this on the local dev box. It generates keys, state, and deployment
# artifacts for each host, then prints the commands to run on each machine.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPLOY="$ROOT/deploy-p43"
BIN="$ROOT/target/release/dllmd"
CLI="$ROOT/target/release/dllm"

# Host configuration
KANSAS_HOST="root@165.245.194.225"
NEWYORK_HOST="root@147.182.129.159"
LAPTOP_HOST="acidito@192.168.1.189"

KANSAS_API="http://165.245.194.225:7337"
NEWYORK_API="http://147.182.129.159:7337"

P2P_PORT=7444
FIXTURE_PORT=8081

echo "=== P4.3 Physical Validation ==="
echo ""

# Step 1: Build
echo "--- Step 1: Build release binaries ---"
cd "$ROOT"
cargo build --release --bin dllmd --bin dllm 2>&1 | tail -2
echo "dllmd: $(ls -lh "$BIN" | awk '{print $5}')"
echo ""

# Step 2: Create deployment directory
echo "--- Step 2: Create deployment artifacts ---"
rm -rf "$DEPLOY"
mkdir -p "$DEPLOY"/{kansas,newyork,laptop}

# Step 3: Generate transport keys for each node
echo "--- Step 3: Generate transport keys ---"
for node in kansas newyork laptop; do
    "$BIN" --help > /dev/null 2>&1 || true
    # We'll use dllmd to generate the key on first start.
    # For now, create placeholder dirs.
    cp "$BIN" "$DEPLOY/$node/dllmd"
done

# Copy fixture runtime
cp "$SCRIPT_DIR/fixture-runtime.py" "$DEPLOY/fixture-runtime.py"
chmod +x "$DEPLOY/fixture-runtime.py"

echo "Artifacts ready in $DEPLOY/"
echo ""

# Step 4: Print deployment instructions
cat << 'INSTRUCTIONS'

=== DEPLOYMENT INSTRUCTIONS ===

For each host, copy the deploy directory and run the setup commands below.
All commands assume the deploy directory is at ~/p43-deploy/.

--- KANSAS (165.245.194.225) - Bootstrap + Forwarding Member ---
Copy files:
  scp deploy-p43/kansas/dllmd deploy-p43/fixture-runtime.py KANSAS_HOST:~/

On Kansas:
  # Start the fixture runtime
  python3 ~/fixture-runtime.py 8081 &
  FIXTURE_PID=$!

  # Generate transport key and start dllmd as bootstrap + forwarding member
  # Create an owner network first
  DLLMD_BIND=0.0.0.0:7337 \
  DLLMD_STATE=~/p43-state.json \
  DLLMD_OWNER_KEY=~/p43-owner.key \
  DLLMD_NETWORK=p43-test \
  DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
  DLLMD_ADMISSION_LIMIT=4 \
  DLLMD_P2P_ENABLED=true \
  DLLMD_P2P_PORT=7444 \
  DLLMD_P2P_KEY=~/p43-transport.key \
  DLLMD_P2P_RESERVE=false \
  ./dllmd &

--- NEW YORK (147.182.129.159) - Forwarding Member ---
Copy files:
  scp deploy-p43/newyork/dllmd deploy-p43/fixture-runtime.py NEWYORK_HOST:~/

On New York:
  # Start the fixture runtime
  python3 ~/fixture-runtime.py 8081 &
  FIXTURE_PID=$!

  # Start dllmd with a replica state (no owner key)
  DLLMD_BIND=0.0.0.0:7337 \
  DLLMD_STATE=~/p43-state.json \
  DLLMD_NETWORK=p43-test \
  DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
  DLLMD_ADMISSION_LIMIT=4 \
  DLLMD_P2P_ENABLED=true \
  DLLMD_P2P_PORT=7444 \
  DLLMD_P2P_KEY=~/p43-transport.key \
  DLLMD_P2P_BOOTSTRAP="/ip4/165.245.194.225/tcp/7444" \
  DLLMD_P2P_RESERVE=false \
  ./dllmd &

--- LAPTOP (192.168.1.189) - Edge Node ---
Copy files:
  scp deploy-p43/laptop/dllmd deploy-p43/fixture-runtime.py LAPTOP_HOST:~/

On Laptop:
  # Start dllmd as edge node (no forwarding, reserves a forwarded path)
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

--- LOCAL DEV BOX - Owner/Test Client ---
  # The local dev box acts as owner. After bootstrap is up, create bindings:
  # 1. Get each node's PeerId from GET /v1/peer-network/status
  # 2. Bind transport: dllm bind-transport <node-pubkey> <peer-id> <gen> <expiry>
  # 3. Set forwarder: dllm set-forwarder <node-pubkey> <max-reservations>
  # 4. Assign model: dllm assign <model> <node-pubkey>
  # 5. Distribute state to members

INSTRUCTIONS

echo ""
echo "=== After deploying and starting all nodes, run the test scenarios below ==="
echo ""
cat << 'TESTS'

=== PHYSICAL TEST SCENARIOS ===

SCENARIO 1: Direct path
  curl -s http://KANSAS_API/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d '{"model":"test-model","stream":true}'
  Expected: streaming SSE response with chunks, status 200.
  Check: GET /v1/peer-network/status → path = "direct"

SCENARIO 2: Forwarded path
  # Block direct UDP between edge and serving node
  iptables -A INPUT -p udp --dport 7444 -j DROP  # on serving node
  curl ... (same request)
  Expected: still works via forwarded path.
  Check: path = "forwarded", selected_forwarder is set.
  # Clean up: iptables -D INPUT -p udp --dport 7444 -j DROP

SCENARIO 3: Concurrency ceiling
  ADMISSION_LIMIT=4, send 6 concurrent requests:
  for i in $(seq 1 6); do
    curl -s ... &  # background each
  done
  wait
  Expected: 4 succeed (200), 2 rejected (429 or error frame)

SCENARIO 4: Cancellation
  Start a slow request (X-Chunk-Count: 20, X-Delay-Ms: 500)
  After 1 second, Ctrl+C the curl client.
  Check serving node logs: runtime request aborted, permit released.
  Check: GET /v1/peer-network/status → cancelled_streams incremented.

SCENARIO 5: Deadline
  Send a request with a short deadline (use X-Delay-Ms: 1000, deadline 500ms).
  Expected: deadline error frame returned, upstream cancelled.

SCENARIO 6: Live authorization
  1. Start a slow streaming request.
  2. While streaming, revoke the serving member's transport binding:
     dllm revoke-transport <node-pubkey>
  3. Distribute new state to the serving node.
  Expected: active stream terminates, new request from same peer rejected.

SCENARIO 7: Recovery
  1. Kill the forwarding node on Kansas.
  2. Check that the edge node reselects New York.
  3. Request succeeds via the new forwarder.
  Expected: reselections incremented, new forwarder shown in status.

SCENARIO 8: Restart / address change
  1. Restart the New York daemon.
  2. The edge node must reconnect without manual addressing.
  Expected: next request succeeds, same PeerId, possibly different address.

SCENARIO 9: Security observation
  - ss -tlnp: no runtime port exposed publicly (8081 only on 127.0.0.1)
  - tcpdump -i any port 22: no peer traffic on SSH
  - ls ~/p43-*.key: owner key only on owner machine, not on members
  - No separate relay binary running

TESTS

echo ""
echo "=== After tests, clean up ==="
cat << 'CLEANUP'

On each host:
  kill %1  # dllmd
  kill %2  # fixture runtime (if started)
  rm -f ~/p43-*.json ~/p43-*.key ~/dllmd ~/fixture-runtime.py
  iptables -D INPUT -p udp --dport 7444 -j DROP 2>/dev/null || true

Verify ports closed:
  ss -tlnp | grep -E '7337|7444|8081'  # should be empty

CLEANUP
