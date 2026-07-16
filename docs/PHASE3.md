# Phase 3 Engineering Log

Last updated: 2026-07-15

This is the authoritative engineering log for WAN hardening and remote
management described in `docs/dllm-proposal.md`. Phase 2 completed with the
decision recorded in `phase2-results/final-summary.json`.

## Phase 3 scope

Phase 3 hardens the private orchestration system for administration and
inference across untrusted and higher-latency networks. It does not introduce
public discovery or community compute.

In scope:

- authenticated remote management with roles and scoped credentials;
- NAT traversal with authenticated relay fallback;
- owner transfer, recovery, and control-plane backup;
- placement draining and upgrade safety;
- per-credential fairness and quotas, plus runtime-supported batching; and
- repeatable LAN, metro, cross-country, direct, and relayed benchmarks.

Out of scope: listed discovery, public membership workflows, community resource
markets, distributed expert placement, multimodal input, and revival of
distributed dense layer stages without a new feasibility decision.

## Acceptance criteria

1. Remote management requires TLS on non-loopback interfaces and enforces
   viewer, operator, and admin roles with least-privilege credentials.
2. A member behind NAT can establish an authenticated path automatically, and
   relay fallback preserves network and peer identity without exposing runtime
   ports publicly.
3. Ownership can transfer to an eligible member without an unsigned state
   interval, and an encrypted, integrity-checked backup can restore the control
   plane under a documented recovery procedure.
4. A placement can drain existing work while rejecting new work, and a rolling
   daemon upgrade preserves service when another ready replica exists.
5. Per-credential quotas and fair admission prevent one client from consuming
   all available concurrency. Batching is enabled only when the selected
   runtime supports it and measurements show a benefit.
6. The same workload is benchmarked on LAN, metro, and cross-country paths,
   including direct and relayed transport where available, with latency,
   throughput, traffic, failure, and recovery evidence.
7. CLI and UI workflows expose role, transport, drain, backup, quota, and
   upgrade state without revealing credential secrets.

## Milestones

- [x] P3.0: establish scope, acceptance criteria, threat boundaries, and
  implementation order.
- [x] P3.1: implement remote-management roles, scoped credentials, credential
  rotation, and audit-safe status surfaces.
- [ ] P3.2: implement authenticated peer transport, NAT traversal, and relay
  fallback with explicit direct-versus-relayed state.
- [x] P3.3: implement owner transfer, encrypted control-plane backup, and
  documented recovery validation.
- [x] P3.4: implement placement draining and validate a replica-safe rolling
  daemon upgrade.
- [ ] P3.5: implement per-credential quotas and fair admission, then evaluate
  runtime-supported batching.
- [ ] P3.6: run the LAN, metro, and cross-country direct and relay benchmark
  matrix.
- [ ] P3.7: expose Phase 3 operations in the CLI and UI, run the full acceptance
  suite, and record the machine-readable Phase 3 decision.

Milestones are marked complete only when their implementation and required
evidence are recorded here. A partially complete milestone keeps an unchecked
box and states what remains.

## P3.0 kickoff

The first security boundary is the management API. The Phase 2 daemon accepts
one bearer token with unrestricted management authority. Phase 3 replaces that
model with ordered viewer, operator, and admin roles while retaining the legacy
token as an admin credential for configuration compatibility.

The implementation order is:

1. enforce least-privilege remote-management credentials;
2. establish authenticated direct and relayed peer transport;
3. make state custody recoverable and ownership transferable;
4. add draining before upgrade orchestration;
5. isolate client admission with quotas and fair scheduling; and
6. benchmark the hardened system across the required network matrix.

Remote credentials do not authorize inference, peer transport, or owner-key
access. Those remain separate trust domains. Non-loopback listeners continue to
require TLS and inference authentication in addition to management credentials.

## P3.1 remote-management authorization

The first P3.1 slice implements ordered `viewer`, `operator`, and `admin` roles.
Viewer credentials can read status and run the non-mutating placement preview.
Operator credentials additionally manage assignments and publish hardware
profiles. Admin credentials additionally issue invitations and revoke members.
A valid credential below a route's required role receives HTTP 403. Missing or
unknown credentials receive HTTP 401.

`DLLMD_MANAGEMENT_CREDENTIALS` accepts a JSON array of token and role objects.
For example:

```json
[
  {"token":"status-secret","role":"viewer"},
  {"token":"automation-secret","role":"operator"},
  {"token":"owner-secret","role":"admin"}
]
```

The legacy `DLLMD_MANAGEMENT_TOKEN` remains an admin credential. Duplicate
tokens deterministically receive their highest configured role, and empty
tokens never authorize a request. Additional-network configuration accepts the
same `management_credentials` array. Startup rejects a non-loopback listener if
the primary or any additional network lacks a non-empty management credential.

Dynamic credentials are persisted separately from signed network state when
`DLLMD_MANAGEMENT_CREDENTIALS_PATH` is configured. The file contains SHA-256
token digests rather than bearer secrets and is written with mode `0600` on
Unix. `POST /v1/management/credentials` creates a named credential and returns
its randomly generated 256-bit token once. `GET /v1/management/credentials`
lists credential ID, label, role, creation time, and revocability without the
token or digest. `DELETE /v1/management/credentials/{credential-id}` revokes a
persisted credential immediately. Configured and legacy credentials are marked
non-revocable because configuration remains their source of truth. Revocation
refuses to remove the last admin credential.

The CLI exposes these operations as `dllm credentials`,
`dllm create-credential LABEL ROLE`, and
`dllm revoke-credential CREDENTIAL_ID`. Automated coverage proves the role
matrix, one-time secret exposure, digest-only persistence, mode `0600`, live
revocation, and persistent revocation after registry reload. This completes
P3.1.

## P3.2 authenticated peer transport

The first P3.2 slice separates daemon-to-daemon traffic from public inference.
When `DLLMD_PEER_API_KEY` is configured, peers use dedicated
`/v1/peer/health`, `/v1/peer/health/runtime`, and
`/v1/peer/chat/completions` routes. Requests must carry the peer bearer secret,
the signed network ID, and the caller node public key. The receiving daemon
rejects an unknown network or a caller that is neither the owner nor a signed
member. Runtime ports remain loopback-only behind the receiving daemon.

Member state now optionally carries an owner-signed `relay_endpoint` alongside
the direct endpoint. Health checks and replica routing probe the direct path
first, then the relay path. Management status reports `local`, `direct`, or
`relay` for the selected node path. The join CLI accepts
`--relay-endpoint URL`; existing joins and networks without a relay endpoint
retain their Phase 2 behavior.

Automated coverage proves authenticated peer headers are forwarded, missing
identity is rejected, direct failure selects a ready relay, and member inference
uses the dedicated peer route. Automatic NAT candidate discovery, a maintained
relay service, connection freshness and replay protection, and physical WAN
evidence remain before P3.2 can be marked complete.

## P3.3 ownership and recovery

`dllm transfer-owner NEW_OWNER_KEY --old-owner-endpoint URL` transfers authority
only to a current signed member. The transition advances the generation,
promotes the member, retains the old owner as a member, replaces the local owner
key, and signs the resulting state directly with the new owner key. There is no
unsigned intermediate state. The operation is offline so state and key files
can be backed up before authority changes.

`dllm backup OUTPUT --passphrase-file FILE` creates a versioned encrypted
archive containing persisted signed state, the owner key, and the optional
credential registry. Argon2 derives the archive key and ChaCha20-Poly1305
provides authenticated encryption. Archives and recovered private files use
mode `0600` on Unix. `dllm restore INPUT --passphrase-file FILE` authenticates
and decrypts the archive, verifies the signed state, and verifies the owner key
against that state before writing recovery files. Tests cover wrong-passphrase
rejection and a complete restore load. This completes P3.3.

## P3.4 placement draining and upgrade safety

Placements carry an owner-signed `ready` or `draining` lifecycle. Operators use
`POST /v1/placements/{placement-id}/drain` to drain and `DELETE` on the same
route to resume. The CLI exposes `dllm drain PLACEMENT_ID` and
`dllm resume PLACEMENT_ID`. Draining is idempotent and advances the signed
generation only on change.

Replica selection excludes draining placements from new requests. A request
already assigned to the placement retains its admission permit and replica
lease until its response stream closes. Automated replica validation drains the
preferred replica and proves the next request moves to another ready replica,
which validates the routing portion of a rolling daemon upgrade. This completes
P3.4.

## P3.5 inference fairness and quotas

`DLLMD_INFERENCE_CREDENTIALS` accepts named inference credentials with an
independent `max_in_flight` limit. Tokens are hashed before comparison and are
separate from management credentials. Each request acquires its credential
quota before the daemon-wide admission permit, and both permits remain held
until a streaming response closes. The legacy `DLLMD_API_KEY` remains a single
credential with the global admission limit.

Automated coverage exhausts one client's quota, observes a labeled HTTP 429,
and proves a second credential still completes through the same globally
available runtime. Runtime batching evaluation remains before P3.5 can be
marked complete.
