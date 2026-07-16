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
