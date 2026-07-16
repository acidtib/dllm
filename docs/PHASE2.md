# Phase 2 Engineering Log

Last updated: 2026-07-15

This is the authoritative engineering log for Orchestration, replicas, and
broader hardware described in `docs/dllm-proposal.md`. Phase 1 completed with
the decision recorded in `phase1-results/final-summary.json`.

## Phase 2 scope

Phase 2 extends the validated private-network MVP without reintroducing the
distributed layer-stage path rejected in Phase 0.

In scope:

- multiple models and multiple networks per daemon;
- whole-model replicas with health-aware, load-aware request routing;
- hardware benchmark profiles and automatic placement recommendations;
- CPU-only whole-model workers and auxiliary CPU workloads;
- experimental heterogeneous CPU/GPU pipelines only when measurements justify
  them;
- an additional dense architecture, beginning with Gemma; and
- placement preview, compatibility explanations, and capacity planning in the
  UI.

The laptop hardware matrix includes an Intel Kaby Lake-R GT2 (UHD Graphics 620)
integrated GPU. Acceleration support is not assumed. Runtime source probes and a
measured benchmark will determine whether the laptop uses an Intel GPU backend
or the portable CPU path.

Out of scope: WAN relay hardening, remote-management roles, owner transfer,
public discovery, distributed expert placement, multimodal input, and revival
of distributed dense layer stages without a new feasibility decision.

## Acceptance criteria

1. Two or more ready whole-model replicas serve one model through a single API,
   and routing avoids unavailable replicas while preferring lower observed load.
2. Hardware profiles record runtime compatibility, memory, measured throughput,
   and capacity for the desktop NVIDIA GPUs and laptop CPU and Intel UHD 620.
3. Placement preview gives a deterministic recommendation or a precise
   incompatibility explanation without mutating signed state.
4. A CPU-only whole-model worker completes streaming and non-streaming inference
   with recorded benchmark evidence.
5. At least two model IDs can be placed and listed without duplicate logical
   model entries, including a validated dense Gemma variant.
6. One daemon can manage at least two isolated networks with independent signed
   state, membership, assignments, credentials, and status.
7. The CLI and UI expose replica state, placement preview, compatibility, and
   capacity information.

## Milestones

- [x] P2.0: establish scope, acceptance criteria, and implementation order.
- [ ] P2.1: complete replica routing and physical concurrent-replica evidence.
  The routing core and automated tests are complete. Physical evidence remains.
- [ ] P2.2: publish signed hardware profiles for the desktop and laptop. The
  laptop source and device probe and profile protocol are complete. Publishing
  measured physical profiles for both machines remains.
- [x] P2.3: implement placement preview, compatibility explanations, and
  capacity recommendations in the management API and CLI. Web UI exposure is
  tracked separately in P2.7.
- [x] P2.4: benchmark the laptop Vulkan and CPU runtime candidates, select the
  exact backend, and validate CPU-only streaming and non-streaming inference.
- [x] P2.5: add and validate a dense Gemma manifest and serve at least two model
  IDs without duplicate logical entries.
- [ ] P2.6: manage two isolated networks in one daemon with independent state,
  credentials, membership, assignments, and status.
- [ ] P2.7: expose replicas, placement preview, compatibility, and capacity in
  the Web UI, then run the complete Phase 2 acceptance suite.

Milestones are marked complete only when their implementation and required
evidence are recorded here. A partially complete milestone keeps an unchecked
box and states what remains.

## P2.0 kickoff

Phase 2 begins with replica semantics and hardware discovery. Existing Phase 1
state already permits the same model to be assigned to several nodes, but the
router selects only the first placement. That behavior is not replica routing
and is the first implementation gap.

The implementation order is:

1. make whole-model replicas health-aware and load-aware;
2. define signed node capabilities and measured benchmark profiles;
3. add read-only placement preview and capacity explanations;
4. validate the laptop CPU and Intel GPU runtime paths;
5. add the Gemma manifest and runtime validation; and
6. introduce per-network state isolation in one daemon.

## P2.1 replica routing

The first replica-routing slice is implemented. Model listing now collapses replica assignments
into one logical OpenAI model entry. Request routing filters replicas by runtime
readiness, tracks in-flight requests per placement, selects the least-loaded
ready placement, and uses placement ID as a deterministic tie-breaker. The load
lease is acquired before the upstream request and remains held until its response
stream ends. Prometheus output exposes current in-flight load by placement ID.

Tests cover readiness-based failover, least-in-flight selection, authenticated
member routing, streaming completion, admission saturation, and logical model
listing. This completes the routing core, but physical concurrent-replica
benchmarking remains open.

## P2.2 laptop runtime source and device probe

The physical laptop is `acidito` with an Intel Core i7-8550U, four cores and
eight threads, AVX2 and FMA, 15 GiB RAM, and 12 GiB available during the probe.
Its integrated GPU is PCI device `8086:5917`, Intel Kaby Lake-R GT2 (UHD
Graphics 620), using the `i915` kernel driver.

Vulkan exposes the GPU successfully through Mesa 26.1.4 as a Vulkan 1.4.354
integrated device. The installed Rusticl OpenCL 3.0 platform exposes zero
devices, so OpenCL is not a usable backend in the current configuration.

The exact pinned llama.cpp revision from Phase 0,
`505b1ed15ca80e2a19f12ff4ac365e40fb374053`, contains the `GGML_VULKAN` backend,
documents Linux Vulkan builds, and supports selecting backend devices at
runtime. The exact backend candidates are therefore llama.cpp Vulkan for the
UHD 620 and llama.cpp AVX2 CPU as the portable fallback. Final selection remains
pending until both paths build and run a controlled benchmark. The probe record
is `phase2-results/p20-hardware-probes/laptop.json`.

## P2.3 signed profiles and placement preview

Signed network state now carries at most one current hardware profile per node.
A profile records CPU topology and features, system and available memory,
accelerators, runtime revision and backend compatibility, and integer-scaled
prefill and decode benchmark measurements. Publishing a changed profile advances
and re-signs the network generation. Identical publication is idempotent, and
member revocation removes its profile.

The management API accepts profiles at `POST /v1/hardware-profiles` and provides
read-only recommendations at `POST /v1/placements/preview`. Preview filters by
architecture, backend, and available memory, then ranks compatible nodes by
measured decode rate, memory headroom, and node key. Every incompatible result
states the missing runtime support or exact memory deficit. Preview returns the
generation it evaluated and does not mutate signed state.

The CLI exposes the same operations as `dllm publish-profile PROFILE_FILE` and
`dllm preview`. Automated coverage proves profile publication advances the
generation, a compatible measured backend is selected, and preview leaves the
generation unchanged.

## P2.4 laptop runtime selection and Gemma validation

The pinned llama.cpp revision was built with its Vulkan and AVX2 CPU backends,
copied to the laptop with its shared libraries, and executed successfully. It
identified the UHD 620 as `Vulkan0` through the Intel Mesa driver. The selected
Gemma artifact is `ggml-org/gemma-3-1b-it-GGUF` Q4_K_M at revision
`f9c28bcd85737ffc5aef028638d3341d49869c27`, with SHA-256
`8ccc5cd1f1b3602548715ae25a66ed73fd5dc68a210412eea643eb20eb75a135`.

Five repetitions with 128 prompt tokens and 64 generated tokens measured:

| Backend | Prompt tok/s | Decode tok/s |
|---|---:|---:|
| AVX2 CPU, 4 threads | 72.75 | 18.65 |
| UHD 620 Vulkan, all layers | 78.25 | 6.74 |

Vulkan improved prompt throughput by 7.56 percent but reduced decode throughput
by 63.87 percent. DLLM therefore selects the AVX2 CPU backend for this laptop and
model. This is a whole-model CPU worker, not a heterogeneous pipeline.

The CPU server returned HTTP 200 for streaming and non-streaming chat
completions. Non-streaming produced `DLLM CPU works.` in 0.891 seconds at 20.55
decode tok/s. Streaming produced the same text, emitted `[DONE]`, transferred
2,086 bytes, and completed in 0.443 seconds at 21.07 decode tok/s. The runtime
was stopped after validation. Machine-readable evidence is in
`phase2-results/p24-laptop-runtime/summary.json`.

The exact Gemma manifest is parsed by the runtime test suite. A management API
test places Qwen and Gemma simultaneously, adds a second Qwen replica, and
proves `/v1/models` returns exactly two sorted logical model IDs. Together with
the physical Gemma server validation, this completes P2.5.
