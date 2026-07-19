# DLLM Hardware Auto-Detection and GPU/Context Auto-Tuning

**Follow-up to `docs/dllm-proposal.md` sections 3, 9.1, and Phase 2**

---

## 1. Purpose

`DLLMD_GPU_LAYERS` and `DLLMD_CONTEXT_SIZE` are env vars with hardcoded
fallbacks (`38` and `2048`, `crates/dllm-daemon/src/main.rs:402,414,441`)
regardless of the model or the GPU's actual VRAM. More broadly, the entire
hardware-profile and benchmark system in the protocol
(`HardwareProfile`, `HardwareBenchmark` in
`crates/dllm-protocol/src/lib.rs:112-157`) is populated by hand today: the
only way data gets into it is `dllm`'s `PublishProfile` command
(`crates/dllm-cli/src/main.rs:434`), which reads a JSON file an operator
wrote themselves. Nothing in the codebase probes an accelerator or runs a
benchmark.

This document scopes closing that gap: automatic accelerator detection, a
joint gpu_layers/context_size calculation, and a real benchmark that
validates it — turning `HardwareBenchmark`'s existing but always-empty
fields (`decode_tokens_per_second_milli`, `peak_memory_bytes`,
`context_size`, `concurrency`) into real, measured data.

This is a plan, not an implementation.

---

## 2. Current state

- `gpu_layers` and `context_size` are independent env vars with fixed
  defaults, set once at daemon startup, with no relationship to the model
  being loaded or the hardware running it.
- `HardwareProfile.accelerators` (`AcceleratorCapability.memory_bytes`) and
  `HardwareProfile.benchmarks` exist as protocol types and are already
  consumed by placement scoring (`crates/dllm-daemon/src/api.rs:1480-1560`,
  the placement-preview endpoint), but nothing populates them automatically.
  An operator must hand-write and publish a profile for placement scoring to
  have anything real to work with.
- Model manifests (e.g. `manifests/qwen2.5-14b-instruct-q4_k_m.yaml`) do not
  carry per-layer memory or tensor dimension data described in proposal
  section 9.1. That data exists already, self-described, in the GGUF file's
  own header — llama.cpp parses it to load the model in the first place —
  so it does not need to be added to the manifest schema.

---

## 3. Target end state

A node loading a model for the first time on a given backend, with no
operator configuration beyond picking the model:

1. Detects available accelerators and free VRAM using the same device query
   llama.cpp already performs to load a model (no second detection path).
2. Reads layer count, per-layer weight size, and per-token KV-cache cost
   straight from the GGUF header.
3. Computes a `gpu_layers`/`context_size` pair instantly: maximize layers
   offloaded to GPU first, then fit the largest context size in whatever
   VRAM remains. Never trades GPU layers away to preserve a larger context.
4. Starts the worker on that instant estimate — no delay waiting for a
   benchmark.
5. In the background, runs a short real prompt-and-decode pass at that
   estimate, measures actual peak memory and decode tok/s, and steps
   `gpu_layers` or `context_size` down if the estimate was too close to OOM.
6. Caches the result as a `HardwareBenchmark` entry keyed by model + backend,
   so every later load of that same pair reuses it instantly instead of
   recalculating or re-benchmarking.

An operator can still override either value individually via
`DLLMD_GPU_LAYERS` / `DLLMD_CONTEXT_SIZE` or `PublishProfile`; an explicit
value always wins over the computed one.

---

## 4. Components

Implemented by `docs/superpowers/plans/2026-07-18-dllm-gpu-autoconfig.md`,
except member-node benchmark publishing during join-then-activate startup
(`spawn_runtime_activation`) and full `CpuCapability`/`AcceleratorCapability`
auto-detection, both left as explicit follow-up work.

### 4.1 Accelerator/VRAM detection

Enumerate backends and free memory via the device query already performed by
`llama-cpp-4` (already a dependency) rather than adding a parallel detection
path (`nvidia-smi`/`vulkaninfo` parsing, NVML bindings, etc.).

### 4.2 Instant joint calculation

Read from the target GGUF file's header:

- layer count and per-layer weight size (quantization-aware, since the
  header describes the artifact as quantized);
- embedding dimension and KV-head count, to derive per-token KV-cache cost.

Combine with detected free VRAM and a safety margin to solve for
`gpu_layers` and `context_size` together: maximize layers on GPU first, fit
context in the remainder. Zero added startup delay — this runs before the
worker starts.

### 4.3 Background micro-benchmark

After the worker is serving on the instant estimate, run a short real
prompt + decode pass at that (gpu_layers, context_size) pair. Measure actual
peak memory and decode tok/s. If the estimate was too close to OOM, step
either value down and re-verify. Write the result into
`HardwareBenchmark` (`model`, `backend`, `context_size`, `concurrency`,
`prompt_tokens_per_second_milli`, `decode_tokens_per_second_milli`,
`peak_memory_bytes` — all fields already exist, just unpopulated today).

### 4.4 Caching and reuse

Cache keyed by model + backend. A later load of the same pair on the same
node reuses the cached result immediately rather than recalculating or
re-benchmarking. Staleness uses the existing `observed_at_unix` field on
`HardwareProfile`.

### 4.5 Resolution order

1. Explicit `DLLMD_GPU_LAYERS` / `DLLMD_CONTEXT_SIZE` or a `PublishProfile`-
   supplied value — wins individually if set, so an operator can pin one and
   let the other auto-tune.
2. Cached benchmark result for this model + backend on this node.
3. Instant joint calculation (section 4.2).
4. Today's hardcoded `38` / `2048`, only if detection itself fails.

---

## 5. Non-goals for this document

- Concurrency auto-tuning. `HardwareBenchmark.concurrency` exists as a field
  but is out of scope here; the same mechanism could extend to it later.
- Changing placement *node selection* logic — this only affects what
  happens once a node has already been chosen to run a model.
- Removing `PublishProfile` or the manual override path.
- Manifest schema changes — GGUF header data is used instead, deliberately
  avoiding a new manifest field set that would need to stay in sync with
  every quantization variant.

---

## 6. Open risks

- **Restart-on-correction.** If the background benchmark finds the instant
  estimate was too aggressive, the worker needs a safe reload path at a
  lower `gpu_layers`/`context_size` rather than continuing to risk OOM.
- **Benchmark load on a busy node.** The micro-benchmark competes with real
  inference traffic if a node starts serving real requests before the
  background benchmark finishes.
- **VRAM drift.** A node's free VRAM can change between the cached benchmark
  and now (another process claims memory). Staleness handling needs a
  policy, not just a timestamp.
- **GGUF header parsing surface.** Reading layer/tensor metadata directly
  from GGUF files means DLLM depends on that format's header staying stable
  and readable independent of full model load — needs confirming this is
  already exposed by `llama-cpp-4`'s bindings, not something requiring new
  parsing code.

---

## 7. Relationship to the proposal

This directly implements the "Measured claims" product principle (section
3: performance claims based on reproducible benchmarks, not projected
capability) and finishes what Phase 2 in section 16 already claims as done
("Hardware benchmark profiles and automatic placement recommendations") —
that phase's `:DONE` tag covers the protocol types and placement-scoring
consumer, but not automatic population of the data they consume.
