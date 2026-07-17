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

- [x] P4.0: select the embedded peer transport after local, separate-NAT,
  node-integrated forwarding, recovery, resource, and dependency validation.
- [x] P4.1: add owner-signed bindings between DLLM node identities and rotating
  transport endpoint identities, including revocation and replay protection.
- [x] P4.2: embed discovery, NAT traversal, and encrypted forwarding roles in
  `dllmd`, prove automatic selection of an eligible participating node, and
  remove SSH and separately deployed relays from the supported peer path.
- [x] P4.3: carry authenticated health, inference streaming, cancellation, and
  deadlines over the embedded transport with bounded concurrent streams and
  automatic path recovery.
- [x] P4.4: implement separate unlisted and listed discovery without allowing
  discovery records to grant membership or override owner-signed state.
- [x] P4.5: implement owner-approved membership and request-access policy that
  is independent of compute membership and enforces explicit resource budgets.
- [x] P4.6: add discovery hosting controls, rate limits, moderation, abuse
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

The P4.0 evaluation matrix requires and now proves:

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

Iroh's standard relay shape did not satisfy the embedded-node requirement, so
rust-libp2p was evaluated because Hivemind and Petals demonstrate the relevant pattern:
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
node-integrated forwarding acceptance criterion. The rust-libp2p comparison
below tests the required embedded `dllmd` forwarding role against its DHT,
AutoNAT, DCUtR, and Circuit Relay v2 composition. Detailed evidence
from the rejected experiment remains in
`results/phase4-results/p40-iroh-evaluation/summary.json`.

### Rust-libp2p node-integrated spike

The 2026-07-16 local spike uses rust-libp2p 0.56.0 in one executable with two
ordinary node configurations. A forwarding-eligible node runs Circuit Relay
v2 alongside Identify, Kademlia, Ping, and the DLLM request protocol. Edge
nodes run the relay client, DCUtR, AutoNAT, Kademlia, and the same authenticated
request protocol. There is no separate relay executable or SSH data path.

The three-process test passed encrypted circuit reservation, forwarding, and
application exchange. The forwarding node reported `CircuitReqAccepted`, and
the authorized peer received `hello-through-node`. A fourth peer could create
a transport circuit but was rejected by the DLLM application identity check.
Transport reachability therefore did not grant DLLM membership.

The spike found that a forwarding node must add its observed address as an
external address after Identify, otherwise clients cannot retain the
reservation. The first restart test also exposed missing listener recovery.
The edge node now detects forwarding connection loss, retries the node, and
recreates its circuit reservation after a new Identify exchange. A repeated
local test passed before and after restart without duplicate recovered
reservations.

The same executable then passed the separate-network test with Colorado as the
dialer, an ordinary forwarding-enabled node in Kansas, and an ordinary edge
node in New York. The forwarded request completed in 626 ms. After stopping
Kansas for two seconds, New York restored its reservation without restarting;
recovery was observed within 5.715 seconds and the next request completed in
630 ms. SSH only installed, started, inspected, and removed the processes. It
did not carry peer traffic.

The optimized binary is 15,988,904 bytes. Short-run systemd memory accounting
reported 2,306,048 bytes for Kansas and 2,609,152 bytes for New York. Both
remote services, deployed binaries, and temporary firewall rules were removed
after the test.

A subsequent DHT slice separated bootstrap from forwarding eligibility. An
ordinary Kansas node provided only the initial Kademlia route. An ordinary New
York node joined through Kansas and published the
`/dllm/forwarding/v1` provider key. Colorado queried Kansas and discovered only
the New York forwarding peer in 301 ms. The bootstrap node did not implicitly
become eligible. This proves capability discovery, but the probe still needs to
connect to the discovered address and apply signed policy when multiple
providers are returned.

The edge now resolves and connects to the selected provider by peer identity
using addresses learned through Kademlia. In the physical topology, Colorado
received only the Kansas bootstrap address, discovered New York, and connected
directly to its QUIC endpoint in 354 ms. No operator supplied the New York
address to the edge.

A local two-provider test added a fail-closed policy filter. Both providers
were returned by the DHT, but the edge connected only to the explicitly
approved peer. A policy naming a peer that was not among the providers returned
an error and made no connection. This probe input models the decision boundary;
it is not a substitute for the owner-signed forwarding policy required by
P4.1 and P4.2.

### P4.0 final decision

Rust-libp2p 0.56.0 is selected for Phase 4. The final probe automatically
discovers an eligible forwarding node through Kademlia, applies a fail-closed
owner-policy boundary, resolves the selected peer by identity, reserves a
Circuit Relay v2 path, exchanges the authenticated application request, and
upgrades the path through DCUtR when direct QUIC succeeds. Diagnostics report
the bootstrap peer, discovered providers, selected forwarding peer,
reservation, forwarded path, direct path, failed connections, resource-limit
rejections, and reselection events.

The final physical topology used an ordinary bootstrap node and an ordinary
forwarding-enabled node in Kansas, an ordinary edge node in New York, and the
Colorado development machine behind residential NAT. The complete automatic
path passed without carrying peer traffic over SSH or deploying a separate
relay service. Blocking direct Colorado-to-New York UDP retained the forwarded
path. Restoring it allowed DCUtR to report migration from `forwarded` to
`direct`.

Recovery passed for forwarding daemon restart, replacement by a different
eligible forwarding peer, and the same peer identity returning on a different
address. A live authorization change accepted a request before revocation and
rejected the same transport identity afterward without restarting the edge
node. Owner-signed endpoint-binding tests cover rotation, expiry, revocation,
and stale-generation replay rejection.

The stream evaluation defines explicit start, chunk, cancel, and end frames
with deadline and concurrency enforcement. Ten sequential physical requests
passed in 8.665 seconds. During the concurrent physical slice, five circuits
were accepted and three excess circuits were rejected with
`ResourceLimitExceeded`, demonstrating a bounded forwarding ceiling.

The selected optimized probe is 15,900,416 bytes. After the load slice, peak
cgroup memory was 3,350,528 bytes for the Kansas forwarding role and 3,403,776
bytes for the New York listener; process RSS was approximately 14 MiB. Removing
the rejected iroh candidate reduced the resolved transport tree from 1,308 to
751 lines and duplicate dependency roots from 57 to 29. The combined-candidate
clean release build took 230.864 seconds and used approximately 778 MiB of
temporary build output; the selected-only clean build is recorded in the
structured result at 101.190 seconds and 536,584,111 bytes of build output.

All remote services, deployed probe binaries, and temporary firewall rules
were removed after validation. Detailed evidence is in
`results/phase4-results/p40-libp2p-evaluation/summary.json`. P4.0 is complete;
P4.1 will turn the validated signed binding and policy model into persisted
owner-signed DLLM state.

## P4.1 persisted transport identity bindings

The signed network state now carries one active libp2p peer ID per DLLM node
and durable revocation tombstones. Each binding records its own monotonically
increasing generation, server-assigned issue time, and expiry. The state
validator rejects malformed libp2p peer IDs, invalid lifetimes, unknown nodes,
duplicate node or endpoint bindings, and any active binding superseded by a
tombstone.

Binding rotation is an owner-authorized control-plane mutation. It requires the
exact next binding generation, tombstones the previous endpoint, advances the
network state generation, and signs the complete state with the owner key. A
stale or skipped binding generation fails closed, and a tombstoned endpoint
cannot be rebound to any node. Explicit endpoint revocation
also advances and signs the state while retaining the endpoint and generation
as a tombstone. Member revocation automatically tombstones that member's active
transport identity before removing it.

`dllm bind-transport` and `dllm revoke-transport` expose these mutations through
admin-only daemon routes. Transport authorization accepts an endpoint only when
the presented libp2p peer ID matches the current binding and has not expired.
Signed-state consumers can require a generation newer than their cached state,
which rejects replay of an otherwise valid older owner signature.

Automated tests cover signing, malformed state, rotation, expiry, endpoint
mismatch, explicit revocation, member revocation, stale binding generations,
stale signed-state generations, persistence, daemon restart, and admin routing.
An end-to-end CLI and daemon test passed bind, rotation,
stale replay rejection, restart, and revocation. The same test passed on the
ordinary Kansas VPS host with two actual libp2p peer IDs. The remote daemon,
binaries, state, keys, and listener were removed afterward. Detailed evidence
is in `results/phase4-results/p41-transport-bindings/summary.json`.

## P4.2 embedded discovery and forwarding

`dllmd` now starts the selected rust-libp2p stack in-process. Each enabled node
runs encrypted TCP and QUIC transports, Noise authentication, Identify,
Kademlia, AutoNAT, DCUtR, ping, and Circuit Relay v2 client and server
behaviors. An ordinary node may bootstrap routing, publish forwarding
capability, provide bounded forwarding, reserve a forwarded path, or combine
those roles. There is no separate communication process.

Forwarding eligibility is owner-signed state keyed by the DLLM node identity.
The policy supplies a maximum reservation count. A node resolves eligible DLLM
identities through their signed P4.1 libp2p bindings, queries the DHT for live
providers, intersects the two sets, and selects deterministically. A provider
record without signed eligibility is observable but cannot be selected. Member
revocation also removes its forwarding policy.

Member daemons can load a verified signed-state replica without the owner
private key. Owner mutations on a replica fail closed. Local node identity,
libp2p identity, and the owner identity remain distinct. Startup rejects a
transport key unless its peer ID matches the active owner-signed binding for
the configured local DLLM node.

Operators initialize a transport identity with `dllm init-transport`, bind it
with `dllm bind-transport`, and manage eligibility with `dllm set-forwarder`
and `dllm remove-forwarder`. `GET /v1/peer-network/status` reports bootstrap
peers, discovered providers, the selected forwarding member, reservation and
path state, failures, reselections, errors, and listen addresses.

The obsolete `dllm-relay` and `dllm-tunnel` executables were removed. Join no
longer accepts a relay endpoint, and direct HTTP failure cannot select the
legacy relay field retained only for signed-state decoding compatibility. SSH
is not a peer transport.

Automated tests run multiple ordinary peer nodes in one process. They prove
that an unapproved DHT provider is not selected, a policy-approved provider
receives the reservation, and loss of the selected provider automatically
chooses another eligible node. A complete local `dllmd` topology additionally
proved owner-key-free state replicas, daemon diagnostics, restart recovery,
and replica mutation rejection.

The physical topology used an ordinary owner/bootstrap node and an ordinary
forwarding member in Kansas, a second ordinary forwarding member in New York,
and a Colorado edge behind residential NAT. The edge discovered both eligible
providers and initially reserved through Kansas. After Kansas forwarding was
stopped, it selected New York and restored the forwarded path in 16.507
seconds. Neither forwarding member held the owner key. SSH only deployed,
administered, inspected, and removed the nodes. All services, binaries, keys,
state, and listeners were removed after validation. Detailed evidence is in
`results/phase4-results/p42-embedded-peer-network/summary.json`.

## P4.3 authenticated inference transport (implementation complete, physical validation pending)

### Protocol

The application protocol runs on libp2p bidirectional streams negotiated as
`/dllm/inference/1`. A custom `ConnectionHandler` wrapping
`ReadyUpgrade<StreamProtocol>` negotiates the protocol on inbound and outbound
connections. Fully negotiated `Stream` objects are forwarded to application code
via a `NetworkBehaviour` that communicates through `mpsc` channels with the
swarm event loop.

### Wire format

A length-delimited binary framing layer (`dllm-transport::protocol`) carries
versioned messages with bounded sizes. Each frame is:

```
[1 byte: protocol version = 1]
[1 byte: message type]
[4 bytes: payload length (big-endian u32)]
[payload bytes]
```

Message types: HealthRequest, HealthResponse, InferenceStart, ResponseStart,
ResponseChunk, Cancel, End, Error. Bounds: 1 MiB max frame, 32 max headers,
256 B max header name, 4 KiB max header value, 1 MiB max body/chunk,
300 s max deadline horizon.

### Authorization

`dllm-transport::auth::AuthView` wraps a `watch::Receiver<Arc<NetworkState>>`
for live authorization. It maps a Noise-authenticated libp2p `PeerId` through
owner-signed transport bindings and enforces membership, expiry, rotation,
revocation, and state generation. The view is updated atomically when new
signed state is distributed.

### Peer refactoring

`PeerNodeHandle` is now cloneable and carries an `mpsc::UnboundedSender` for
stream commands. The `Behaviour` struct includes the new `stream_handler::Behaviour`,
which negotiates `/dllm/inference/1` and emits inbound/outbound stream events.

### Daemon integration

`ApiState` carries an optional `PeerClient` and `AuthView`. When peer transport
is enabled, `resolve_member_transport` tries authenticated libp2p health before
falling back to HTTP. `resolve_runtime` resolves transport PeerIds for member
placements. `proxy` dispatches to `proxy_peer` for libp2p-routable targets,
mapping chunked responses from the peer `Stream` into an Axum streaming body
with a 60 s default deadline.

A background dispatcher task (`spawn_dispatcher`) reads stream events from the
peer handle. Inbound streams are authorized through `AuthView`, health requests
receive a `HealthResponse`, and inference requests are proxied only to the
local runtime via reqwest with deadline, admission, and cancellation handling.

`PeerNodeHandle` exposes `update_diagnostics()` backed by a shared
`watch::Sender`, and the stream dispatcher increments counters for active
streams, rejections, cancellations, deadline expirations, protocol failures,
and auth failures.

### Automated test coverage

78 tests pass (19 protocol, 18 auth, 2 peer, 2 evaluation, 6 protocol types,
3 runtime, 27 daemon, 7 lifecycle integration). The protocol, auth, and
lifecycle/limits matrices are covered. Routing and recovery tests require
physical validation with multiple `dllmd` nodes.

### Remaining

- Physical acceptance matrix (9 scenarios: direct, forwarded, concurrency,
  cancellation, deadline, live-auth, recovery, restart, security observation)
- Physical cleanup on test hosts

## P4.4 listed and unlisted discovery

### Discovery mode

Each node controls whether it publishes reachability records to the Kademlia DHT
via a `DiscoveryMode` enum with `Listed` (default) and `Unlisted` variants. The
mode is local configuration (`DLLMD_P2P_DISCOVERY_MODE` environment variable), not
owner-signed state. Discovery records publish reachability only; they do not grant
membership, inference access, or forwarding eligibility. Authorization is always
enforced through the owner-signed `NetworkState` via `AuthView`, independent of
discovery mode.

Listed nodes that are forwarding-eligible (owner-signed forwarding policy) publish
`/dllm/forwarding/v1` to the DHT so edges can discover and reserve through them.
Unlisted nodes never publish provider records. A private mode (no DHT participation
at all) is deferred to a later milestone.

### Startup guardrails

An unlisted node that is also forwarding-eligible is rejected at startup. Edges
discover forwarders exclusively through the DHT, so an unlisted forwarder would be
invisible — a broken configuration. An unlisted node with no bootstrap peers emits
a warning: it can still receive inbound connections from peers that know its
address, but it cannot join the DHT on its own.

### Diagnostics

`GET /v1/peer-network/status` now includes `discovery_mode` (`"listed"` or
`"unlisted"`) and `published_discovery` (whether the node has published a provider
record to the DHT). The probe binary (`libp2p-node-probe`) accepts
`--discovery-mode` on the `Forwarder` subcommand.

### Automated test coverage

87 tests pass across the workspace. Three new tests cover the discovery mode
behavior: `unlisted_nodes_do_not_publish` (integration — two nodes, unlisted node
never appears in DHT provider results), `unlisted_forwarder_rejected_at_startup`
(unit — `start_peer_node` returns `Err` for the invalid combination), and
`both_discovery_modes_enforce_authorization` (unit on `AuthView` — authorization
is structurally independent of discovery mode).

Existing tests continue to pass with the new `discovery_mode: Listed` default on
all `PeerNodeConfig` constructions. The P4.3 local demo script works unchanged.

### Remaining

- Physical validation with multiple nodes across the VPS hosts

## P4.5 owner-approved membership and resource budgets

### Access requests

Nodes that discover the network through P4.4 can now request membership. `POST /v1/access-requests` is a public, unauthenticated endpoint that accepts a `SignedAccessRequest` — an Ed25519 self-signed proof that the requester holds the node key they claim. The signature is verified against the `node_pubkey` embedded in the request, proving key possession without requiring an owner-issued token.

Pending requests are stored in `PersistedState` alongside redeemed token IDs. They are unsigned, locally-mutable bookkeeping that does not bump the signed state generation. The owner (or an admin-tier credential holder) reviews pending requests via `GET /v1/access-requests`, approves with `POST /v1/access-requests/approve` (which bumps generation, pushes a `Member`, and signs), or denies with `POST /v1/access-requests/deny` (which drops the pending entry without a signed-state mutation).

`approve_access_request` only grants network membership — it never implicitly creates a `ResourceBudget` or forwarding entry. Those remain separate owner actions, preserving the independence between compute contribution and inference consumption.

### Resource budgets

A `ResourceBudget` is owner-signed state on `NetworkState` (new field `resource_budgets: Vec<ResourceBudget>`) that grants a member the right to consume inference through the peer transport. Each budget specifies:

- `max_in_flight` — concurrent request cap (enforced via a per-member `tokio::sync::Semaphore`)
- `max_requests_per_window` and `window_seconds` — rate limit enforced via a sliding window of Unix timestamps

`validate_state` checks that budget entries reference known nodes (`BudgetNodeUnknown`), contain no duplicates (`DuplicateBudgetNode`), and allow at least one request (`EmptyResourceBudget`). No validation rule ties a `ResourceBudget` to `ForwardingPolicy` or `ModelAssignment` — a member can be a pure consumer (budget, no compute), a pure contributor (compute, no budget), or both.

`POST /v1/resource-budgets` (admin-tier) upserts a budget entry. `DELETE /v1/resource-budgets` removes one. Both bump the signed state generation. `revoke_member` strips the member's resource budget alongside forwarding policy, model assignments, placements, hardware profiles, and transport bindings — full revocation tears down everything; partial grants stay independent while membership exists.

### Enforcement

A `BudgetEnforcer` in `crates/dllm-daemon/src/budget.rs` holds per-member `Arc<Semaphore>` instances and sliding-window timestamp vectors. It reconciles with the signed `NetworkState` whenever new state arrives. Two enforcement gates check budgets:

1. **libp2p dispatcher** (`peer_service.rs`): After `AuthView::authorize()` resolves the caller's `node_pubkey`, `serve_inference` calls `budget_enforcer.try_admit()` before the global admission semaphore. No budget entry → fail closed.

2. **HTTP proxy path**: The per-credential `InferenceIdentity` quota and global admission semaphore continue to govern HTTP inference clients. The `BudgetEnforcer` is an additional, independent gate for DLLM network members consuming inference through the peer transport.

Budget enforcement is isolated per member: Node A's quota exhaustion does not affect Node B.

### CLI

New subcommands: `dllm request-access <owner-endpoint>` (self-signs and submits), `dllm list-access-requests`, `dllm approve-access <node-key-file> --endpoint <addr>`, `dllm deny-access <node-key-file>`, `dllm set-budget <node-key-file> --max-in-flight <n> --max-per-window <n> --window-seconds <n> --owner`, and `dllm remove-budget <node-key-file> --owner`.

`GET /v1/inference-policy` now returns both `credentials: Vec<InferencePolicy>` (local bearer-token quotas) and `member_budgets: Vec<ResourceBudget>` (owner-signed per-member budgets), so operators can see both systems.

### Automated test coverage

107 tests pass across the workspace (up from 87). New tests cover:

| Area | Tests | What they prove |
|------|-------|----------------|
| Protocol | 5 | `SignedAccessRequest` verification and tamper detection, `validate_state` rejects `BudgetNodeUnknown`/`DuplicateBudgetNode`/`EmptyResourceBudget` |
| Store | 10 | submit → approve member appears; submit → deny no state mutation; duplicate/redundant submission rejected; existing member rejected; budget set/remove with idempotency; budget rejects unknown node and empty; `revoke_member` strips budget; budget independent of forwarding policy |
| Budget enforcer | 5 | `NoBudget` when absent, permits up to `max_in_flight`, sliding window exhaustion, per-member isolation, stale budget removal on reconcile |
| Existing | 87 | All P4.0–P4.4 tests continue to pass with the new `resource_budgets` and `budget_enforcer` fields |

### Remaining

- Physical validation: Colorado edge node discovers the network via P4.4, submits an access request to the Kansas owner node, is approved, and is rejected on inference until a budget is explicitly granted.
- Enforcement physical test: budget exhaustion isolation across members, sliding window rollover.

## P4.6 discovery hosting controls, rate limits, moderation, and audit records

### Discovery hosting controls

`dllmd` now composes `libp2p::connection_limits::Behaviour` into the swarm,
protecting against unbounded inbound connections. Limits are local daemon
configuration (`DLLMD_P2P_MAX_ESTABLISHED_INCOMING`,
`DLLMD_P2P_MAX_ESTABLISHED_PER_PEER`, `DLLMD_P2P_MAX_PENDING_INCOMING`), not
owner-signed state — resource protection, not authorization.

Kademlia storage is bounded through an explicit `MemoryStoreConfig` with capped
`max_records`, `max_provided_keys`, `max_providers_per_key`, and
`max_value_bytes`. A DHT hosting toggle (`DLLMD_P2P_DHT_HOSTING`) controls
whether a node runs `kad::Mode::Server` (answers queries, stores others'
records) or `kad::Mode::Client` (queries only). Previously every node was
hardcoded to `Server`. The toggle is independent of `DiscoveryMode`: an
unlisted node can still be a DHT server; a listed node can be a client-only
participant.

### Rate limits

`POST /v1/access-requests` is now gated by a configurable sliding-window rate
limiter keyed by source IP, enabled by wiring
`into_make_service_with_connect_info::<SocketAddr>()` in both the TLS and
non-TLS serving paths. A second, pubkey-keyed cooldown prevents immediate
resubmission after denial. The pending access request queue is capped at 1,000
entries; exceeding it returns `PendingQueueFull`. The `RateLimiter` module
follows the same per-key timestamp-vector pattern established by
`BudgetEnforcer` in P4.5.

### Moderation

Owner-signed `MembershipBan` entries on `NetworkState` let the owner
pre-emptively refuse known-bad identities. `validate_state` rejects a ban that
overlaps an active member (`BanTargetIsMember`): banning requires revocation
first. `submit_access_request` checks the ban list before queuing.
`AuthView::authorize()` checks the ban list after membership validation, so a
banned identity is rejected at the libp2p inference stream level as well.

Admin routes `POST /v1/moderation/bans` and `DELETE /v1/moderation/bans`
manage the ban list. CLI: `dllm ban-node <key> --reason <text> --owner` and
`dllm unban-node <key> --owner`.

### Abuse reporting

`AbuseReport` is a local, unsigned queue in `PersistedState` (matching the
`pending_access_requests` pattern). `POST /v1/abuse-reports` and
`GET /v1/abuse-reports` are admin-tier routes. The submitter's membership is
validated in the store layer (`AbuseReportNotMember`). CLI: `dllm
report-abuse <subject-key> --category ... --note ...` and `dllm
list-abuse-reports`.

### Audit records

An append-only JSONL audit log (`crates/dllm-daemon/src/audit.rs`) records
every mutation: access request submit/approve/deny, budget set/remove, member
revoke, forwarding policy changes, transport bind/revoke, model
assign/unassign, placement drain/resume, owner transfer, ban/unban, abuse
report submit, and rate-limit rejections. Each entry captures timestamp,
actor, action, target, and outcome. The log is best-effort via an `mpsc`
channel to a dedicated writer task; failures drop entries silently rather than
blocking mutations. Files rotate past 10 MB. Route: `GET /v1/audit-log`
(viewer-tier, paginated by `?since=<unix>&limit=<n>`). CLI: `dllm audit-log
--since <unix> --limit <n>`.

### Automated test coverage

119 tests pass across the workspace (up from 107). New tests cover:

| Area | Tests |
|------|-------|
| Protocol | ban overlapping active member rejected; ban targeting owner rejected; non-member ban accepted; `MembershipBan`/`AbuseReport` roundtrip |
| Rate limiter | admits up to max; window rollover; isolated keys; prune removes stale |
| Audit log | writes entries; rotation creates new file; dropped sender does not panic |
| Existing | all P4.0-P4.5 tests pass with new `banned` field and `ApiState` fields |

### Remaining

- Physical validation: VPS-based testing of rate limits, connection limits,
  ban enforcement, abuse report submission, and audit log observability.
- Cleanup of remote services, binaries, state, keys, and firewall rules after
  validation, per `AGENTS.md`.
