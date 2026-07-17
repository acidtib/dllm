#!/usr/bin/env bash
# P4.3 local demo — starts a two-node libp2p topology with a fixture runtime.
# One command:  ./scripts/p43-local-demo.sh
# Then hit:     curl http://127.0.0.1:7338/v1/chat/completions \
#                   -H "Content-Type: application/json" \
#                   -d '{"model":"test-model","stream":true}'
# Stop:         kill %1 %2 %3
set -e
BIN=target/release/dllmd
CLI=target/release/dllm
TDIR=/tmp/p43-demo

echo "=== P4.3 Local Demo ==="

# Build if needed
[ -x "$BIN" ] || cargo build --release --bin dllmd --bin dllm
[ -x "$CLI" ] || cargo build --release --bin dllm

# Clean previous
pkill -9 dllmd 2>/dev/null || true
pkill -9 -f fixture-runtime 2>/dev/null || true
sleep 1

rm -rf "$TDIR"
mkdir -p "$TDIR"

# ---- Start fixture runtime ----
echo "[1/5] Starting fixture runtime on :8081..."
python3 scripts/fixture-runtime.py 8081 > "$TDIR/fixture.log" 2>&1 &
sleep 1
curl -sf http://127.0.0.1:8081/health > /dev/null || { echo "Fixture failed"; exit 1; }
echo "  Fixture: OK"

# ---- Generate keys ----
echo "[2/5] Generating keys..."
N1_PEER=$("$CLI" --transport-key "$TDIR/n1-transport.key" init-transport 2>/dev/null)
N2_PEER=$("$CLI" --transport-key "$TDIR/n2-transport.key" init-transport 2>/dev/null)
echo "  N1 PeerId: ${N1_PEER:0:20}..."
echo "  N2 PeerId: ${N2_PEER:0:20}..."

# ---- Generate N2 node key ----
DLLMD_BIND=127.0.0.1:7350 DLLMD_STATE="$TDIR/n2-tmp.json" DLLMD_OWNER_KEY="$TDIR/n2-node.key" \
  DLLMD_NETWORK=tmp DLLMD_ADMISSION_LIMIT=1 "$BIN" > /dev/null 2>&1 &
TMP_PID=$!; sleep 2
N2_PUBKEY=$(curl -sf http://127.0.0.1:7350/v1/status | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)['network']['state']['owner_pubkey']))")
kill $TMP_PID; wait $TMP_PID 2>/dev/null; rm -f "$TDIR/n2-tmp.json"

# ---- Create state (N1 without P2P) ----
echo "[3/5] Creating signed state..."
DLLMD_BIND=127.0.0.1:7337 DLLMD_STATE="$TDIR/state.json" DLLMD_OWNER_KEY="$TDIR/owner.key" \
  DLLMD_NETWORK=p43-demo DLLMD_RUNTIME_URL=http://127.0.0.1:8081 DLLMD_ADMISSION_LIMIT=4 \
  "$BIN" > "$TDIR/owner.log" 2>&1 &
OWNER=$!; sleep 2

N1_KEY=$(curl -sf http://127.0.0.1:7337/v1/status | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)['network']['state']['owner_pubkey']))")

# Join N2, assign model, bind transports, set forwarding
TOKEN=$(curl -sf -X POST http://127.0.0.1:7337/v1/invitations -H "Content-Type: application/json" -d '{}')
curl -sf -X POST http://127.0.0.1:7337/v1/members/join -H "Content-Type: application/json" \
  -d "{\"token\":$TOKEN,\"node_pubkey\":$N2_PUBKEY,\"node_endpoint\":\"http://127.0.0.1:7338\"}" > /dev/null

curl -sf -X POST http://127.0.0.1:7337/v1/assignments -H "Content-Type: application/json" \
  -d "{\"model\":\"test-model\",\"node_pubkey\":$N1_KEY}" > /dev/null

python3 -c "
import json, urllib.request
def bind(key, peer):
    body = json.dumps({'node_pubkey': key, 'transport_peer_id': peer, 'binding_generation': 1, 'expires_at_unix': 2000000000})
    req = urllib.request.Request('http://127.0.0.1:7337/v1/transport-bindings', method='POST', data=body.encode())
    req.add_header('Content-Type', 'application/json')
    urllib.request.urlopen(req, timeout=5)
bind($N1_KEY, '$N1_PEER')
bind($N2_PUBKEY, '$N2_PEER')
" 2>/dev/null

curl -sf -X POST http://127.0.0.1:7337/v1/forwarding-policy -H "Content-Type: application/json" \
  -d "{\"node_pubkey\":$N1_KEY,\"max_reservations\":4}" > /dev/null
curl -sf -X POST http://127.0.0.1:7337/v1/forwarding-policy -H "Content-Type: application/json" \
  -d "{\"node_pubkey\":$N2_PUBKEY,\"max_reservations\":4}" > /dev/null

kill $OWNER; wait $OWNER 2>/dev/null
echo "  State: gen=$(curl -sf http://127.0.0.1:7337/v1/status 2>/dev/null | python3 -c 'import sys,json; print(json.load(sys.stdin)["network"]["state"]["generation"])' 2>/dev/null || echo '?')"

# ---- Start both nodes with P2P ----
echo "[4/5] Starting P2P nodes..."
DLLMD_BIND=127.0.0.1:7337 DLLMD_STATE="$TDIR/state.json" DLLMD_OWNER_KEY="$TDIR/owner.key" \
  DLLMD_NODE_KEY="$TDIR/owner.key" DLLMD_NETWORK=p43-demo DLLMD_RUNTIME_URL=http://127.0.0.1:8081 \
  DLLMD_ADMISSION_LIMIT=4 DLLMD_P2P_ENABLED=true DLLMD_P2P_PORT=7444 \
  DLLMD_P2P_KEY="$TDIR/n1-transport.key" \
  "$BIN" > "$TDIR/n1-p2p.log" 2>&1 &
N1=$!; sleep 3
echo "  N1 (bootstrap): PID=$N1"

DLLMD_BIND=127.0.0.1:7338 DLLMD_STATE="$TDIR/state.json" \
  DLLMD_OWNER_KEY="$TDIR/NONEXISTENT" DLLMD_NODE_KEY="$TDIR/n2-node.key" \
  DLLMD_NETWORK=p43-demo DLLMD_ADMISSION_LIMIT=2 DLLMD_P2P_ENABLED=true DLLMD_P2P_PORT=7445 \
  DLLMD_P2P_KEY="$TDIR/n2-transport.key" \
  DLLMD_P2P_BOOTSTRAP="/ip4/127.0.0.1/tcp/7444/p2p/$N1_PEER" \
  "$BIN" > "$TDIR/n2-p2p.log" 2>&1 &
N2=$!; sleep 5
echo "  N2 (edge):      PID=$N2"

# ---- Done ----
echo "[5/5] Ready!"
echo ""
echo "  Inference via libp2p:"
echo "    curl -s http://127.0.0.1:7338/v1/chat/completions \\"
echo "      -H 'Content-Type: application/json' \\"
echo "      -d '{\"model\":\"test-model\",\"stream\":true}'"
echo ""
echo "  Diagnostics:"
echo "    curl -s http://127.0.0.1:7338/v1/peer-network/status | python3 -m json.tool"
echo "    curl -s http://127.0.0.1:7337/v1/peer-network/status | python3 -m json.tool"
echo ""
echo "  Stop:  kill $N1 $N2; kill \$(pgrep -f fixture-runtime)"
