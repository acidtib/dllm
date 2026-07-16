# Phase 1 Engineering Log

Last updated: 2026-07-15

This is the authoritative log for the Private-network MVP described in
`docs/dllm-proposal.md`. Phase 1 follows the Phase 0 outcome recorded in
`phase0-results/p10-baselines/summary.json`.

## Phase 1 scope

Phase 1 delivers a private, invite-only network and whole-model inference
orchestration. The first supported model is the validated dense Qwen variant
from Phase 0, served through the selected whole-model runtime path.

Distributed layer-stage execution is not a Phase 1 dependency. The stock
llama.cpp RPC path is not the production distributed runtime because Phase 0
disproved the required failure and stage-runtime assumptions.

In scope:

- `dllmd` and `dllm` node identity and local management;
- private invite-only network creation, join, leave, and revocation;
- owner-led signed control-plane state and explicit owner-offline behavior;
- whole-model placement and health reporting;
- the validated dense pipeline mode only if its runtime is available as an
  explicit, non-default compatibility path;
- streaming and non-streaming chat completions and model listing;
- bounded admission with explicit saturation errors;
- minimal onboarding, assignment, placement, and health UI; and
- explicit request failure when a required worker disappears.

Out of scope for this phase: public discovery, WAN relay hardening, owner
transfer, multiple networks per daemon, replicas, automatic placement
recommendations, new architecture families, multimodal input, and recovery of
an in-flight request after worker loss.

## Phase 1 acceptance criteria

1. A user creates a private network, generates a scoped join token, joins a
   second machine, assigns the supported model, and receives streamed
   completions without manually configuring peer addresses.
2. The owner can revoke the second node and observe placement becoming
   unavailable or being rebuilt safely.
3. `dllm status --json` exposes complete network, node, worker, placement, and
   health state.
4. Benchmarks report time to first token, decode rate, network traffic, and a
   single-node fallback comparison.

## P1.0 kickoff decision

Phase 1 is started as an orchestration MVP, not as a continuation of the
distributed-runtime experiment. The implementation must preserve a clean
runtime boundary so the whole-model worker can later be replaced or extended
without changing network membership, placement generations, or the API
contract.

## P1.1 initial control-plane decisions

The first implementation will use an owner-led control plane:

- the network creator is the owner and signs membership and placement state;
- node identity is an Ed25519 public key with locally protected private key;
- join tokens are scoped to one network, single-use by default, and
  revocable;
- membership state is generation-numbered and signed;
- owner-offline nodes may continue serving the last valid placement, while
  mutations and placement changes are rejected explicitly; and
- the local CLI and UI use a loopback or local-socket management API.

These are design decisions, not yet implementation claims. The next work item
is to turn them into versioned protocol/state types and tests before adding
inference routing.

## Open decisions before implementation

- Rust workspace and crate boundaries from the proposal;
- persistent state choice, SQLite or another embedded transactional store;
- exact local transport and authentication framing;
- join-token encoding and storage; and
- the whole-model runtime adapter boundary and model manifest format.

## P1.1 implementation

The initial `dllm-protocol` crate now defines schema-versioned network state,
members, signed generations, and scoped single-use join-token data. Signed
state verification checks schema version, non-zero generation, owner identity,
and Ed25519 signature integrity. Tampering and signer-mismatch tests are
included. `cargo test --workspace` passes with three tests.

The `dllmd` crate now provides the first owner-led state store. It creates an
owner identity, issues network-scoped join tokens, redeems each token once,
advances the signed membership generation, revokes members, and can persist the
signed state as JSON. Its redemption, replay protection, and revocation paths
have passing tests. Owner key bytes can now be saved and loaded with strict
32-byte validation, preventing accidental identity changes across restarts.

## Status

P1.1 control-plane protocol and initial daemon state store implemented and
tested. The `dllm` CLI now supports network creation and JSON or human-readable
status from persisted state. A loopback-bound `dllmd` management API now exposes
status, invitation issuance, join redemption, and member revocation. Successful
membership mutations persist the new signed generation.

A loopback smoke test started `dllmd`, issued an invitation through the API,
joined one generated node, and read status back successfully. The response
reported generation 2, one member, and a 64-byte state signature.

The daemon now loads existing signed state and its owner identity on restart.
Owner key files are restricted to mode `0600` on Unix, state writes use a
temporary file and atomic rename, and redeemed token IDs are persisted so a
single-use invitation remains unusable after restart. Invitations are signed by
the network owner, validated before redemption, and may carry an absolute Unix
expiry.

The `dllm` CLI now talks to the management API for status, invitation issuance,
join, and revocation. `dllm status --json` returns network, node, worker,
placement, and aggregate health fields. Worker and placement arrays remain empty
until model assignment is implemented.

The daemon exposes `/v1/models` and `/v1/chat/completions` as streaming-capable
proxies to a configured whole-model runtime. Admission is bounded by
`DLLMD_ADMISSION_LIMIT`; saturation returns HTTP 429, a missing runtime returns
HTTP 503, and upstream connection failures return HTTP 502. The admission permit
is retained until the upstream response stream ends.

A restart smoke test confirmed that the network name and generation survive a
daemon restart, the owner key is 32 bytes with mode `0600`, the full JSON status
shape is available through the CLI, and an unconfigured model endpoint returns
HTTP 503 explicitly.

Management API authentication and remote node transport remain incomplete. The
management listener still defaults to loopback and must not be exposed remotely.

## P1.2 whole-model placement state

Model assignments and whole-model placements are now part of the signed network
generation. Assigning a model to the owner or a joined member creates one
whole-model placement and advances the generation. Repeating the same assignment
is idempotent. Unassignment and member revocation remove affected placements and
advance signed state safely.

The management API exposes assignment creation and removal, and the CLI provides
`assign` and `unassign` commands. The explicit `--owner` target derives the owner
public key from the protected owner key file, avoiding ambiguity between secret
key bytes and node public-key files.

Status now derives worker and placement records from signed placement state. A
placement is `ready` when a whole-model runtime is configured and `unavailable`
otherwise. Any unavailable placement makes aggregate health `degraded`. A smoke
test assigned the validated Qwen model to the owner, advanced the generation from
1 to 2, and reported one unavailable worker and placement because the test daemon
had no runtime configured.

## P1.3 authenticated management and onboarding

The daemon now refuses a non-loopback bind unless `DLLMD_MANAGEMENT_TOKEN` is
configured. Management and inference use separate bearer credentials, with the
inference credential supplied through `DLLMD_API_KEY`. Integration tests prove
that missing credentials receive HTTP 401. The invitation-backed join endpoint
is intentionally outside management authentication because the signed,
single-use, expiring invitation is its authorization credential.

Invitations now carry an owner endpoint covered by the owner's signature. The
CLI can initialize a mode-`0600` node identity, verify an invitation locally,
and contact the signed owner endpoint without a separately configured peer
address. An end-to-end loopback onboarding test created an invitation file,
created a second node identity, joined through the token endpoint, advanced the
network to generation 2, and reported two nodes.

The bundled dashboard is served at `/` and displays network generation, nodes,
placements, workers, and aggregate health. It accepts an optional management
token and stores it in browser local storage for subsequent local requests.

Runtime readiness now requires a successful `/health` probe rather than the
presence of a configured URL. `/metrics` exports inference request, admission
rejection, upstream failure, response byte, and available-permit counters.
Integration tests cover management authentication, inference API keys, bounded
admission, model proxying, byte accounting, and preservation of streaming SSE
content through chat completions.
No Phase 2 work has started.
