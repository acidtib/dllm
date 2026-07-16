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
- [ ] P3.1: implement remote-management roles, scoped credentials, credential
  rotation, and audit-safe status surfaces.
- [ ] P3.2: implement authenticated peer transport, NAT traversal, and relay
  fallback with explicit direct-versus-relayed state.
- [ ] P3.3: implement owner transfer, encrypted control-plane backup, and
  documented recovery validation.
- [ ] P3.4: implement placement draining and validate a replica-safe rolling
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

Automated coverage proves a viewer can read status but cannot mutate an
assignment, an operator cannot issue an invitation, and an admin can issue an
invitation. Credential persistence, live rotation, credential identifiers, and
an audit-safe access-status surface remain before P3.1 can be marked complete.
