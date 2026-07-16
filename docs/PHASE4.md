# Phase 4: Discovery and controlled community networks

Phase 4 replaces the temporary SSH relay path with a peer network embedded in
`dllmd` before adding community discovery. Discovery publishes reachability.
Owner-signed DLLM state continues to decide membership and authorization.

## Acceptance criteria

1. A user can install only `dllmd`, join a network, discover peers,
   authenticate with DLLM node identities, and serve inference without
   deploying a separate communication service, configuring SSH, or exposing a
   runtime publicly.
2. Discovery records provide signed reachability information only. They cannot
   grant membership or override owner-signed DLLM state.
3. Bootstrap, discovery, NAT traversal, and encrypted forwarding are
   capabilities of ordinary participating `dllmd` nodes. Normal operation does
   not require dedicated or third-party infrastructure.
4. Nodes connect directly where possible. When forwarding is necessary, DLLM
   automatically selects an eligible member according to signed policy and
   resource limits.
5. Status and diagnostics report the discovered endpoint, direct or forwarded
   path, forwarding member, connection failures, and path changes.

## Milestones

- [ ] P4.0: select the embedded peer transport after local, separate-NAT,
  node-integrated forwarding, recovery, resource, and dependency validation.
- [ ] P4.1: add owner-signed bindings between DLLM node identities and rotating
  iroh endpoint identities, including revocation and replay protection.
- [ ] P4.2: embed discovery, NAT traversal, and encrypted forwarding roles in
  `dllmd`, prove automatic selection of an eligible participating node, and
  remove SSH and separately deployed relays from the supported peer path.
- [ ] P4.3: carry authenticated health, inference streaming, cancellation, and
  deadlines over the embedded transport with bounded concurrent streams and
  automatic path recovery.
- [ ] P4.4: implement separate unlisted and listed discovery without allowing
  discovery records to grant membership or override owner-signed state.
- [ ] P4.5: implement owner-approved membership and request-access policy that
  is independent of compute membership and enforces explicit resource budgets.
- [ ] P4.6: add discovery hosting controls, rate limits, moderation, abuse
  reporting, and operator-visible audit records.
- [ ] P4.7: expose onboarding, discovery visibility, approval, transport path,
  forwarding member, policy, and failure diagnostics through the CLI and UI, then complete
  the physical acceptance matrix.

Milestones are complete only when their implementation, automated coverage,
physical evidence, diagnostics, and cleanup checks pass. The New York and
Kansas VPS hosts may run ordinary `dllmd` nodes with forwarding eligibility.
The laptop and primary development machine provide distinct edge-network nodes
for direct, forwarded, and path-migration tests. SSH may administer test hosts,
but it must not carry DLLM peer traffic or be required by a node.

## P4.0 embedded transport evaluation

Iroh 1.0.2 is the first candidate. It requires Rust 1.91 and is compatible with
the current Rust 1.96 workspace. The evaluation disables its default metrics,
port-mapping, and Apple data-path features and enables only the ring TLS
provider.

The isolated `dllm-transport` crate proves that two local endpoints negotiate
the DLLM ALPN over an encrypted QUIC connection, exchange a request and
response on a bidirectional stream, expose the authenticated remote endpoint
identity, bind that identity to a distinct DLLM node public key, and reject an
unknown endpoint. The transport identity remains separate from the DLLM
authorization identity to avoid cross-protocol private-key reuse and allow
transport-key rotation.

Iroh exposes endpoint address lookup, direct-path information, and a separate
relay-server implementation. Its direct QUIC transport remains a candidate.
Its standard relay deployment shape does not satisfy DLLM's operational model;
any forwarding role must be embedded in an ordinary `dllmd` node and selected
through the peer network.

The dependency cost is material. Adding the minimal client configuration added
227 packages to the workspace lock resolution on the evaluation machine, and
iroh currently brings a release-candidate Ed25519 dependency alongside DLLM's
stable Ed25519 dependency. Binary size, compile time, dependency duplication,
and upstream API stability must be measured before adoption.

P4.0 remains incomplete until the evaluation proves:

- discovery and forwarding using only ordinary participating `dllmd` nodes;
- forwarded connectivity between nodes behind separate NATs without a
  separately deployed service;
- migration from a forwarded path to a direct path when hole punching succeeds;
- observable and reliable direct-versus-forwarded path reporting;
- streaming inference framing, cancellation, deadlines, and concurrent streams;
- rejection and live revocation of unauthorized DLLM identities;
- recovery after forwarding-node loss, address changes, and daemon restart;
- transport-key rotation through an owner-signed endpoint binding; and
- acceptable release binary size, startup time, memory, and compile cost.

Iroh remains selected only if its components can support that embedded-node
shape without relying on managed infrastructure. Otherwise, rust-libp2p is the
next candidate because Hivemind and Petals demonstrate the relevant pattern:
each process runs the DHT and P2P stack, reachable full peers participate in
routing, and NATed peers discover eligible forwarding peers automatically.
Quinn alone is not a complete alternative because DLLM would need to build
discovery and NAT traversal itself.

### Rejected dedicated-relay experiment

The 2026-07-16 physical slice used the separate `iroh-relay` binary in Kansas,
a peer in New York, and the Colorado development machine behind residential
NAT. It proved encrypted forwarding mechanics and authenticated endpoint
rejection, but its deployment shape is rejected: DLLM users must not deploy a
separate communication service.

Opening the New York QUIC endpoint as an explicit direct candidate reduced
connection setup to 49 ms and reported the IP path active while retaining the
Kansas relay. Dropping inbound UDP to that endpoint made the direct candidate
inactive; the relay remained active and the request completed with an 89 ms
connection setup. Killing the relay process caused systemd to restart it, and
traffic recovered on the first probe. The conservative 7.671 second recovery
measurement includes client startup and a three-second path-observation window.

The optimized probe is 18,933,312 bytes and used 3,715,072 bytes peak RSS as the
New York server. The optimized relay is 12,883,600 bytes and used 3,645,440
bytes peak RSS in the short validation. These are acceptable initial figures,
but sustained-load measurements remain open.

The result remains useful as transport evidence, but it does not advance the
node-integrated forwarding acceptance criterion. P4.0 remains incomplete. The
next comparison must test an embedded `dllmd` forwarding role against
rust-libp2p's DHT, AutoNAT, and Circuit Relay v2 composition. Detailed evidence
from the rejected experiment remains in
`results/phase4-results/p40-iroh-evaluation/summary.json`.
