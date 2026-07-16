# Phase 4: Discovery and controlled community networks

Phase 4 replaces the temporary SSH relay path before adding community discovery.
Discovery publishes reachability. Owner-signed DLLM state continues to decide
membership and authorization.

## Acceptance criteria

1. Two fresh nodes behind ordinary NAT can join, discover each other,
   authenticate with DLLM node identities, establish a direct or relayed
   encrypted connection, and serve inference without SSH configuration or a
   publicly exposed runtime.
2. Discovery records provide signed reachability information only. They cannot
   grant membership or override owner-signed DLLM state.
3. Operators can self-host all required bootstrap, discovery, and relay
   infrastructure. Normal operation does not require a specific third-party
   service.
4. Status and diagnostics report the discovered endpoint, direct or relayed
   path, relay provider, connection failures, and path changes.

## Milestones

- [ ] P4.0: select the embedded peer transport after local, self-hosted relay,
  separate-NAT, recovery, resource, and dependency validation.
- [ ] P4.1: add owner-signed bindings between DLLM node identities and rotating
  iroh endpoint identities, including revocation and replay protection.
- [ ] P4.2: operate self-hosted discovery and relay infrastructure, prove direct
  NAT traversal and encrypted relay fallback, and remove SSH from the supported
  peer path.
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
  relay, policy, and failure diagnostics through the CLI and UI, then complete
  the physical acceptance matrix.

Milestones are complete only when their implementation, automated coverage,
physical evidence, diagnostics, and cleanup checks pass. The New York and
Kansas VPS hosts may provide self-hosted relay and discovery infrastructure.
The laptop and primary development machine provide distinct edge-network nodes
for direct, relay-only, and path-migration tests. SSH may be used to administer
test hosts, but it must not carry DLLM peer traffic or be required by a node.

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

Iroh exposes custom relay maps, a self-hostable `iroh-relay` server, endpoint
address lookup, and remote path information. These APIs cover the required
relay ownership and direct-versus-relayed diagnostics in principle. They are
not yet validated in DLLM across real NAT boundaries.

The dependency cost is material. Adding the minimal client configuration added
227 packages to the workspace lock resolution on the evaluation machine, and
iroh currently brings a release-candidate Ed25519 dependency alongside DLLM's
stable Ed25519 dependency. Binary size, compile time, dependency duplication,
and upstream API stability must be measured before adoption.

P4.0 remains incomplete until the spike proves:

- a self-hosted relay with no n0-operated service dependency;
- relay-only connectivity between nodes behind separate NATs;
- migration from relay to a direct path when hole punching succeeds;
- observable and reliable direct-versus-relayed path reporting;
- streaming inference framing, cancellation, deadlines, and concurrent streams;
- rejection and live revocation of unauthorized DLLM identities;
- recovery after relay loss, address changes, and daemon restart;
- transport-key rotation through an owner-signed endpoint binding; and
- acceptable release binary size, startup time, memory, and compile cost.

If iroh fails those checks, the next candidate is rust-libp2p with QUIC,
Identify, AutoNAT, Circuit Relay v2, DCUtR, Rendezvous, and Kademlia enabled only
as required. Quinn alone is not a complete alternative because DLLM would need
to build discovery, NAT traversal, and relay behavior itself.

### First physical result

The 2026-07-16 physical slice used a self-hosted iroh 1.0.2 relay in Kansas, a
peer in New York, and the Colorado development machine behind residential NAT.
No n0-operated service or SSH data tunnel carried peer traffic. A relay-only
request connected in 89 ms and completed in 190 ms. An unknown transport
identity reached the authenticated QUIC endpoint but was rejected with DLLM
application code 403.

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

This result validates self-hosting, relay-only traffic, authenticated endpoint
authorization, direct selection, direct-loss fallback, path reporting, and
relay-process recovery. P4.0 remains incomplete because the available laptop
did not accept the configured SSH key, so two independently NATed edge nodes
were not tested. The relay also used iroh's development HTTP listener; peer
payloads remained end-to-end encrypted, but production relay TLS is still
required. Detailed evidence is in
`results/phase4-results/p40-iroh-evaluation/summary.json`.
