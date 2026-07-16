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
have passing tests.

## Status

P1.1 control-plane protocol and initial daemon state store implemented and
tested. Authenticated transport, durable owner-key storage, and the CLI remain
next.
No Phase 2 work has started.
