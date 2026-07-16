# Phase 0 Engineering Log

Last updated: 2026-07-15

This is the authoritative log for the runtime feasibility prototype described in
`docs/dllm-proposal.md`. Phase 0 is limited to proving or rejecting distributed
contiguous layer-stage inference. Later product components are out of scope.

## Current repository state

- The repository contains `AGENTS.md`, `docs/dllm-proposal.md`, and this log.
- There is no Cargo workspace, application code, vendored runtime, model
  manifest, test suite, or benchmark harness.
- `docs/dllm-proposal.md` is staged and `AGENTS.md` is untracked. These are
  pre-existing user changes and must be preserved.
- No future crates or packages will be scaffolded until an experiment requires
  them.

## Phase 0 outcome and exit criteria

Phase 0 must end in one of two documented outcomes:

1. A dense Qwen model that does not fit in either target node's usable memory
   completes prefill and autoregressive generation across both nodes. Each node
   holds only its contiguous layer range and stage-local KV cache. Results match
   a trusted single-process baseline within a stated tolerance, measurements
   are reproducible, and runtime strategy and maintenance cost are documented.
2. A required runtime capability is shown to be absent or impractical through
   source inspection and experiments. Required changes, maintenance surface,
   alternatives, and a concrete recommendation are documented.

Phase 1 must not begin automatically after either outcome.

## Environment and hardware

### Local development host: `ergot`

| Item | Observed value |
|---|---|
| Date | 2026-07-15 |
| OS | Linux x86_64, kernel 7.1.3-2-cachyos |
| CPU | Intel Core i7-9700K, 8 cores |
| System memory | 62 GiB total, 56 GiB available at inspection |
| GPUs | Two NVIDIA GeForce GTX 1080 (Pascal, compute capability 6.1), 8 GiB each |
| NVIDIA driver | 580.173.02; operational in the user's interactive session |
| Observed free VRAM | GPU 0: 7,420 MiB; GPU 1: 8,151 MiB at 2026-07-15 17:00 local time |
| CUDA driver API | Reports CUDA 13.0 capability through `nvidia-smi` |
| CUDA toolkit | 13.3, nvcc 13.3.73; does not list `sm_61` as a compilation target |
| GPU topology | GPU0 to GPU1 is `PHB`, crossing a PCIe host bridge; no NVLink |
| Rust | rustc 1.96.0, cargo 1.96.0 |
| CMake | 4.4.0 |

The host driver is loaded and both GPUs are usable from the user's interactive
session. The earlier `nvidia-smi` failure was transient or occurred in a context
without device access. The managed command runner used to maintain this
repository still cannot access NVML, even while the user's shell can. GPU tests
must therefore be run by the user unless that execution isolation is changed.

CUDA toolkit 13.3 is not a viable local compiler for these Pascal GPUs:
`nvcc --list-gpu-arch` begins at `compute_75` and `--list-gpu-code` begins at
`sm_75`, while GTX 1080 requires `sm_61`. A CUDA 12.x compiler toolchain or a
known-compatible prebuilt runtime is required. Driver capability and compiler
target support are separate, so a working 580 driver does not resolve this
toolkit mismatch.

The user confirmed the mismatch directly: `nvcc -arch=sm_61` fails with
`Unsupported gpu architecture 'sm_61'`. NVIDIA's CUDA 13.0 release notes state
that offline compilation and library support for Maxwell, Pascal, and Volta was
removed in CUDA 13.0, while CUDA 12.x remains supported for those architectures.
CUDA 12.9 is therefore the initial compiler target to evaluate:

- <https://docs.nvidia.com/cuda/archive/13.0.0/cuda-toolkit-release-notes/index.html>
- <https://docs.nvidia.com/cuda/archive/12.9.0/cuda-toolkit-release-notes/index.html>

The GPUs have a `PHB` topology, so traffic between them traverses a PCIe host
bridge. There is no NVLink. Kernel logs contain no `NVRM: Xid`, GPU-fallen, CUDA,
or UVM failure beyond the normal proprietary-module taint message at inspection.

The user does not currently have two CUDA machines. The two local GPUs will be
isolated into separate containers and used for the mandatory early two-process
experiments, but they do not satisfy the two-machine Phase 0 exit criterion.

Two available CPU machines may be used later for a direct-LAN transport and
failure-semantics reproduction. That result must be labeled a partial P0.9
validation because it cannot reproduce CUDA worker behavior, GPU memory
pressure, or the documented NVIDIA hardware baseline. Phase 0 may conclude
with the CUDA two-machine criterion explicitly unvalidated if no second GPU
machine becomes available. It must not report the CPU LAN run or two containers
on one host as satisfying that criterion.

Required hardware inventory for each target machine:

- GPU exact model, count, compute capability, total VRAM, and usable VRAM under
  representative idle conditions;
- CPU, RAM, storage space and expected model-storage location;
- operating system, kernel, NVIDIA driver, CUDA runtime/toolkit, and container
  runtime if one will be used;
- LAN link type, negotiated bandwidth, MTU, measured RTT, and whether RDMA is
  available;
- whether either machine can temporarily host the trusted single-process
  baseline using CPU offload or a larger-memory accelerator.

## Runtime source probes

Source probes were performed against immutable upstream revisions. They prove
what the selected source trees express, not that either runtime builds or runs
correctly on the target hardware.

| Runtime | Pinned revision | Local hardware disposition |
|---|---|---|
| llama.cpp | `505b1ed15ca80e2a19f12ff4ac365e40fb374053` | Proceed to a CUDA 12.9.1 build and local RPC allocation probe |
| vLLM | `2bd8957627bfb5668c46f2bc359bef47d371270c` | Do not build on GTX 1080; source requires CUDA compute capability 7.0 or newer |

### llama.cpp

The pinned source and documentation describe an RPC backend that exposes remote
CPU or accelerator devices to one coordinating llama.cpp process. It can
distribute model weights and KV cache across local and remote devices, with a
configurable tensor split. Its server documentation describes `layer` split as
splitting layers and KV across devices.

The source probe establishes the following:

- `src/llama-model.cpp` calculates cumulative device split points, assigns each
  repeating layer to one backend device, and assigns the output layer using the
  same split calculation. In `layer` mode this produces contiguous ranges for a
  two-device positive split.
- The input embedding remains on the coordinator's CPU. This is an explicit
  boundary-tensor exception, not a stage-0 GPU allocation.
- `src/llama-kv-cache.cpp` creates named K and V tensors per model layer and
  selects their buffer from `model.dev_layer(il)`. A remote RPC device should
  therefore own the KV tensors for the layers assigned to it.
- RPC is a ggml backend. The coordinator constructs and schedules the complete
  graph, serializes graph/tensor descriptions, and invokes remote graph
  computation. The remote process is not an independently addressable DLLM
  stage and does not expose DLLM's proposed activation frame.

This is sufficient for a minimal allocation and correctness probe without a
fork. It is not sufficient to accept llama.cpp RPC as the production stage
interface. The next probe must inventory tensor names and buffer sizes in each
RPC process, record graph traffic, and prove that only the boundary activation
crosses between device ranges during steady-state decode.

The RPC README explicitly labels the feature proof-of-concept, fragile, and
insecure. It must only be tested on loopback. LAN execution is blocked because
the published critical advisory affects revisions through b7991 and lists no
patched version. The pinned revision is newer than b7991, but the absence of an
upstream patched-version declaration means commit ordering alone is not
accepted as evidence of remediation.

Relevant upstream material inspected on 2026-07-15:

- <https://github.com/ggml-org/llama.cpp/blob/master/tools/rpc/README.md>
- <https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md>
- <https://github.com/ggml-org/llama.cpp/blob/master/include/llama.h>

A 2026 security advisory reports unauthenticated remote code execution in the
RPC backend for affected revisions. The exact fixed revision must be established
before any LAN test, and the service must not be exposed beyond the controlled
network:

- <https://github.com/ggml-org/llama.cpp/security/advisories/GHSA-j8rj-fmpv-wcxw>

### vLLM

The pinned vLLM source has a closer semantic match to DLLM's desired stages:

- `vllm/model_executor/models/utils.py::make_layers` computes a start and end
  layer for each pipeline rank, constructs only that contiguous range, and
  installs `PPMissingLayer` placeholders outside it.
- `vllm/model_executor/models/qwen2.py` gives embeddings to the first pipeline
  rank and normalization/output responsibility to the last. Non-final ranks
  return `IntermediateTensors` containing both `hidden_states` and `residual`.
  The boundary is therefore two tensors for Qwen2, not a single activation.
- `vllm/distributed/parallel_state.py` sends tensor dictionaries rank to rank
  through PyTorch distributed communication. Single-node execution defaults to
  multiprocessing; multi-node execution defaults to Ray, with a documented
  multiprocessing alternative.
- Weight construction is stage-local at the Python module level. A runtime
  memory trace is still required to rule out loader staging or allocator
  duplication, and KV block ownership still requires an execution probe.

vLLM is not executable on the local Phase 0 hardware at this revision.
`CMakeLists.txt` sets `CUDA_SUPPORTED_ARCHS` to `7.0;7.5;8.0;8.6;8.7;8.9;9.0`.
The GTX 1080 is compute capability 6.1. Adding 6.1 is not treated as a narrow
supported patch because the current kernel and PyTorch dependency surface is
not validated for Pascal. vLLM remains the interface-design comparison and may
be reconsidered only if the later two-machine target meets its supported floor.

Relevant upstream material inspected on 2026-07-15:

- <https://github.com/vllm-project/vllm/blob/main/docs/serving/parallelism_scaling.md>

### Strategy decision gate

Evaluate in this order:

1. Supported upstream APIs or configuration, preferring the smallest patch.
2. A focused maintained fork only if a narrow, reviewable patch satisfies the
   checkpoint and its upstream churn can be measured.
3. A custom stage executor only if existing runtime boundaries cannot provide
   weight loading, arbitrary contiguous execution, KV ownership, and observable
   transfer semantics.

No fork or custom backend is approved yet. For the local GTX 1080 checkpoint,
stock llama.cpp RPC is the only candidate approved for the next executable
probe. The expected first patch is zero lines: enable existing debug logging
and collect process/device allocation evidence. If stock logs cannot inventory
remote tensor residency and graph transfer, the next patch should be a focused
diagnostic-only change in `llama-model.cpp`, `llama-kv-cache.cpp`, and
`ggml-rpc.cpp`, estimated at 100 to 250 lines. No production fork decision may
be made from that instrumentation patch.

### llama.cpp build and device probe

The pinned llama.cpp revision configured and built successfully in
`nvidia/cuda:12.9.1-devel-ubuntu24.04` with `GGML_CUDA=ON`, `GGML_RPC=ON`, and
`CMAKE_CUDA_ARCHITECTURES=61`. CUDA compilation accepted `sm_61` and emitted a
deprecation warning that pre-7.5 offline compilation will be removed in a
future toolkit release. This confirms the intended CUDA 12.9.1 build path but
also makes the toolchain pin mandatory.

Upstream renamed the executable target from `rpc-server` to
`ggml-rpc-server`. The built targets are `ggml-rpc-server`, `llama-cli`, and
`llama-gguf-split`. FlashAttention was disabled at compile time because the
first Pascal execution reached `ggml-cuda/fattn.cu` and aborted when runtime
FlashAttention was not also disabled. The validated baseline therefore requires
both `GGML_CUDA_FA=OFF` at build time and `-fa off` at runtime.

The completed build reports `version: 1 (505b1ed)` and `CUDA : ARCHS = 610`.
A container launched with `--gpus all` enumerated both GTX 1080 devices at
compute capability 6.1. Two simultaneous loopback RPC servers, each restricted
with `CUDA_VISIBLE_DEVICES` to one physical GPU, each enumerated exactly one
8 GiB GTX 1080 and started RPC protocol version 4.0.2.

The first link attempt without `--gpus all` failed because the CUDA image did
not contain the host driver's `libcuda.so.1`. Re-running the incremental link
with GPU injection completed successfully. This is a container invocation
requirement, not a runtime source defect.

The current build unexpectedly provisions llama.cpp UI assets while building
the selected CLI dependency graph. This does not affect the runtime result, but
the reproducible build should disable unrelated UI targets or use a narrower
upstream build configuration if one is available.

### Local RPC allocation probe

The verified Q4_K_M artifacts were loaded through two loopback RPC workers with
the coordinator's local CUDA devices hidden. The successful bounded command used
`-ngl all -sm layer -ts 1,1 -fit off -fa off -c 2048 -ctk f16 -ctv f16`, a
fixed prompt of `Hello`, greedy sampling, one generated token, and single-turn
mode. The process exited normally after generating `Hello`.

The exact assignment from verbose coordinator logs was:

| Owner | Assigned tensors | Model buffer | KV buffer | Compute buffer | Total remote allocation |
|---|---|---:|---:|---:|---:|
| Worker 0, physical GPU 0 | Transformer layers 0 through 24 | 3,917.36 MiB | 200.00 MiB | 208.01 MiB | 4,325.37 MiB |
| Worker 1, physical GPU 1 | Transformer layers 25 through 47 and output layer 48 | 4,231.02 MiB | 184.00 MiB | 208.01 MiB | 4,623.03 MiB |
| Coordinator CPU | Input embedding and mapped boundary tensors | 417.66 MiB mapped | none | 28.01 MiB | Not a remote stage allocation |

The equal tensor split divides 49 offloadable units, the 48 transformer layers
plus the output layer. It is therefore not a 24/24 transformer split. Worker 0
owns 25 transformer layers, while worker 1 owns 23 transformer layers and the
large output tensor. The observed model-buffer asymmetry is expected.

RPC debug logs show two graph executions on each worker, followed by clean
release of each worker's model, KV, and compute buffers. This proves exact
contiguous placement, stage-local KV allocation by layer owner, and successful
prefill/decode execution through the stock RPC backend for this minimal prompt.
It does not yet prove that only one activation crosses the boundary, provide a
tensor-name inventory inside each worker, compare logits against a baseline, or
measure steady-state autoregressive decoding.

Raw results for the current workspace session are under:

```text
/tmp/dllm-phase0-results/p03-rpc-one-token/
/tmp/dllm-phase0-results/p03-rpc-layer-map/
```

These paths are temporary and must be copied into a durable benchmark-results
location before the Phase 0 conclusion.

## Assumptions to prove

| ID | Assumption | Evidence required |
|---|---|---|
| A1 | A runtime can assign an exact contiguous transformer-layer range to each process. | Allocation logs plus tensor inventory by process. |
| A2 | Each process loads only its assigned layer weights, aside from explicitly documented boundary tensors. | Peak and steady memory, runtime allocation trace, and tensor names/sizes. |
| A3 | A serializable activation is the only required forward boundary state between stages. | Captured shape, dtype, byte count, framing, and successful replay into stage 1. |
| A4 | Prefill can cross the same fixed boundary without hidden full-model state on either worker. | Baseline comparison and process memory trace during prefill. |
| A5 | KV cache naturally partitions by layer and remains resident with the process that owns those layers. | Per-layer cache inventory and multi-token decode comparison. |
| A6 | Quantized distributed execution agrees with the same-runtime single-process baseline. | Logit deltas and token sequence under fixed tokenizer, prompt, sampler, and seed. |
| A7 | Serialization and LAN transfer overhead permit useful interactive decoding. | Timings and bytes for prefill and every decode step. |
| A8 | Worker loss, timeout, cancellation, and slow consumption produce explicit bounded failure. | Fault-injection results with upper time bounds and stage identity. |

## Smallest checkpoint experiment

The first executable milestone is one forward pass across two local processes,
not a daemon or API server.

1. Pin a runtime commit and exact model artifact after hardware is known.
2. Produce a trusted single-process run that records the final-token logits.
3. Start process 0 with embeddings and the first contiguous layer range, and
   process 1 with the remaining layers and output head. Boundary ownership may
   change if the runtime requires it, but every exception must be documented.
4. Process 0 tokenizes or accepts fixed token IDs, executes its range, serializes
   the boundary tensor with version, request ID, shape, dtype, payload length,
   and checksum, then sends it over a loopback stream.
5. Process 1 validates the frame, executes its range, and writes final logits.
6. Compare logits with the baseline and record peak/steady RSS, device memory,
   load time, activation bytes, serialization time, transfer time, and execution
   time independently for both processes.
7. Kill and stall each process in separate runs. The harness must terminate with
   a stage-specific error within a configured timeout.

The initial transport should be a Unix socket or loopback TCP, selected to match
the runtime integration with the least code. It is disposable and must not grow
into the production transport during Phase 0.

If neither runtime can expose this split without substantial modification, stop
after a minimal failing probe and write the feasibility result. Do not emulate
stage execution with duplicated full-model workers.

## Correctness methodology

The baseline and distributed run must use the same:

- exact model files, hashes, model revision, and quantization;
- pinned runtime commit and build configuration;
- tokenizer files, tokenizer settings, and chat template;
- prompt token IDs and context length;
- prefill/decode batch settings;
- sampler, temperature, top-k, top-p, repetition settings, and random seed; and
- accelerator math settings that affect determinism.

For the forward-pass checkpoint, persist final-token logits or a lossless digest
plus a comparison artifact sufficient to compute maximum absolute error, mean
absolute error, root-mean-square error, and top-k token agreement. Tolerances
must be derived from repeated single-process runs on the same backend before
judging the distributed result.

For autoregressive generation, compare greedy token IDs first. Sampling tests
come later and must use the same seed. Any nondeterminism is measured with
repeated baseline and distributed trials, not dismissed based on plausible text.

## Benchmark methodology

Use warm and cold runs, report at least five measured repetitions after a warmup,
and retain raw machine-readable records. Report median and range initially;
expand the statistical treatment if variance is material.

Every meaningful milestone records:

- working, mocked, simulated, or hard-coded behavior;
- model and runtime load time;
- process RSS, system RAM, device memory, and KV-cache memory;
- activation payload and framing bytes;
- serialization, deserialization, and transfer time;
- prompt and output token counts;
- time to first token, inter-token latency, and tokens per second;
- per-process CPU/GPU utilization;
- correctness metrics and generated-token differences;
- timeout, cancellation, disconnect, and backpressure results; and
- assumptions proven or disproven.

LAN tests additionally record bandwidth, RTT, MTU, packet loss if observed, and
direct versus relayed path. Phase 0 uses only a direct controlled LAN path.

Compare against practical single-node alternatives on the same available
hardware: the selected quantization with CPU offload when it can run, and a
smaller quantization or smaller model that fits one node. No performance claim
is made unless measurements support it.

## Technical plan and milestone status

| Milestone | Status | Proof produced before continuing |
|---|---|---|
| P0.1 Repository, proposal, environment, assumptions, and methods | Complete | This log and local inspection results |
| P0.2 Hardware inventory and exact model/runtime selection | Complete, CUDA target unavailable | Both LAN machines are inventoried and the exact runtime/model are selected; the laptop is CPU-only, so the two-CUDA-machine target cannot be validated |
| P0.3 Runtime source probes | Complete | Pinned source matrix, Pascal build, device enumeration, exact allocation map, failure probe, and patch estimates |
| P0.4 Two-local-process single forward pass | Complete | Exact logits, token agreement, remote tensor inventory, boundary identity, framing bytes, serialization, socket calls, device copies, round trips, and stage execution timings |
| P0.5 Prefill and stage-local KV cache | Complete | Exact 25-token prefill logits and direct per-worker inventory of all 96 stage-local K/V tensors |
| P0.6 Autoregressive generation | Complete | Exact 16-token greedy agreement across five measured repetitions, plus end-to-end, per-stage, and traffic measurements |
| P0.7 Minimal streaming chat-completions endpoint | Complete | Distributed model listing and one validated OpenAI-shaped SSE chat completion with a documented request subset |
| P0.8 Cancellation, timeout, backpressure, and faults | Complete, acceptance failed | No fabricated success, but worker loss aborts the coordinator generically and worker stalls have no RPC deadline; A8 is disproven |
| P0.9 Controlled two-machine LAN reproduction | Complete, partial CPU validation | Exact two-host greedy reproduction and LAN measurements through a safe SSH tunnel; no second CUDA machine was available |
| P0.10 Baselines, recommendation, and Phase 0 conclusion | Complete | Practical fallback comparison, assumption disposition, runtime decision, maintenance surface, and final Phase 0 outcome |

After each milestone, update this table, record commands and raw-result paths,
run relevant tests, state what was proven, and list what remains unproven.

## P0.4 forward-pass correctness checkpoint

The pinned upstream `llama-debug` target was built without a source patch. It
persists the prompt token IDs and the final-token logits in binary and text
formats. The comparison used identical model artifacts, runtime commit, context
size, FP16 KV types, disabled FlashAttention, 1:1 layer split, prompt, and CUDA
build settings.

The trusted baseline was one llama.cpp process using both CUDA devices directly.
The candidate was one coordinator with local CUDA hidden and two loopback RPC
workers, each restricted to one physical GPU. The prompt `Hello` tokenized to
the single token ID `9707` in every run.

One baseline/candidate pair was used as warmup. Five additional baseline runs
and five additional RPC runs were measured. All 152,064 FP32 final-token logits
were bit-for-bit identical across every run:

| Metric | Result |
|---|---:|
| Maximum absolute error | 0.0 |
| Mean absolute error | 0.0 |
| Root-mean-square error | 0.0 |
| Top-1, top-5, top-10, and top-50 agreement | 100% |
| Argmax token ID | 143399 |
| Argmax logit | 6.90366268157959 |
| Logits SHA-256 | `e97fedee3628699c03a5a5da028b8e9e30a3e66b7511da3168f6f557d858d9a0` |
| Token IDs SHA-256 | `c7f35e833d3235ee678e613f85ffa7aa17a0234c722b0549bf7ec89594a7478f` |

The measured zero-error tolerance is valid for this exact backend, model,
prompt, and build configuration. It must be re-derived if kernels, quantization,
placement, or hardware change.

Durable raw outputs and the machine-readable comparison are stored in
`phase0-results/p04-forward/`. This proves the correctness portion of A6 for the
single forward-pass checkpoint. Stock RPC logs did not expose the remaining
boundary and timing evidence, so the following trace uses a diagnostic-only
logging patch.

### P0.4 RPC boundary trace

A focused diagnostic patch adds logging gated by `GGML_RPC_TRACE`. It does not
change the RPC protocol, scheduling, tensor data, or execution path. The patch
records tensor name, dtype, shape, byte count, and device-copy duration for
server-side set/get operations, plus remote graph size and execution duration.
The patch and raw worker traces are stored in
`phase0-results/p04-boundary/`.

The trace confirms that the only model activation relayed from worker 0 to
worker 1 is `l_out-24`, FP32 with hidden dimension 5,120. For the runtime's
two-token warmup it is `[5120, 2, 1, 1]`, 40,960 bytes. For the measured
30-token prefill it is `[5120, 30, 1, 1]`, 614,400 bytes. Worker 0 copies this
tensor device-to-host in 180 microseconds for the prefill, and worker 1 copies
it host-to-device in 145 microseconds. The coordinator relays the bytes. The
workers do not communicate directly.

The same prefill records these remote graph measurements:

| Worker | Transformer ownership | Graph nodes | Serialized graph description | CUDA graph execution |
|---|---|---:|---:|---:|
| GPU 0 RPC worker | Layers 0 through 24 | 975 | 401,492 bytes | 78,546 microseconds |
| GPU 1 RPC worker | Layers 25 through 47 and output | 902 | 371,900 bytes | 67,914 microseconds |

The second worker returns `result_output`, FP32 `[152064, 1, 1, 1]`, 608,256
bytes, to the coordinator after the final stage. This is a result boundary, not
inter-stage hidden state. Both workers also receive coordinator-generated graph
inputs such as positions and attention masks. Those are stage inputs required
by llama.cpp's coordinator-owned graph, not hidden activations passed from the
first layer range.

The final client trace records serialization and socket-call timing without
changing the protocol. For the 30-token prefill boundary:

| Operation | Measurement |
|---|---:|
| Worker 0 graph serialization | 283 microseconds |
| Worker 0 graph socket send | 58 microseconds |
| Boundary device-to-host copy | 182 microseconds |
| Boundary response socket send | 148 microseconds |
| Boundary get round trip, including ordered worker 0 execution | 65,184 microseconds |
| Boundary frame construction and payload copy | 40 microseconds |
| Boundary request socket send | 93 microseconds |
| Boundary host-to-device copy | 145 microseconds |
| Worker 1 graph serialization | 303 microseconds |
| Worker 1 graph socket send | 57 microseconds |
| Final logits get round trip, including ordered worker 1 execution | 62,302 microseconds |

The boundary set frame is 614,713 bytes: 614,400 payload bytes and 313 bytes of
RPC command, size, tensor descriptor, and offset framing. The graph descriptions
are 401,492 and 371,900 bytes. These graph bytes describe operations and remote
tensor references; they are not activation payloads.

The stock protocol's set and graph-compute commands are fire-and-forget, so
`socket_send_us` means completion of the local blocking socket call, not a
remote acknowledgement. Remote receipt and host-to-device completion are
separately evidenced by the following server event on the ordered connection.
The get round trip includes prior ordered graph execution and is labeled as
such. This limitation is part of the measured result rather than hidden by an
invalid end-to-end transfer estimate.

This trace supplies the named remote tensor inventory and proves the shape,
dtype, payload size, framing, serialization, socket-call, device-copy, round-trip,
and stage-execution portions of A3 and the checkpoint. Together with the exact
logit comparison, P0.4 is complete. The diagnostic patch remains a measurement
artifact and is not accepted as a product interface.

## P0.5 prefill and stage-local KV-cache checkpoint

The trusted baseline and the two-worker RPC candidate evaluated the same fixed
25-token prompt in one prefill batch. Both used the pinned model and runtime,
2,048-token context, FP16 K and V cache, disabled FlashAttention, 1:1 layer
split, and fixed seed. The persisted token-ID files match exactly. The two
152,064-element FP32 final-logit files also match byte-for-byte:

| Metric | Result |
|---|---:|
| Prompt tokens | 25 |
| Maximum absolute error | 0.0 |
| Mean absolute error | 0.0 |
| Root-mean-square error | 0.0 |
| Top-1, top-5, top-10, and top-50 agreement | 100% |
| Logits SHA-256, both paths | `dd1a3f79b1dfed4176947705e35f30361d283020088b8afa2c5ff00702bea17b` |
| Token IDs SHA-256, both paths | `7896a82a931b872ac3fbe8c3aa63b84c2cdfdfe3d78dc1bf79aedbac750775b3` |

The prefill crossed the same fixed activation boundary proven in P0.4.
`l_out-24` was FP32 `[5120, 25, 1, 1]`, with a 512,000-byte payload and a
512,313-byte RPC frame. No additional inter-stage model activation appeared.

The diagnostic RPC trace now records every tensor whose serialized graph name
starts with `cache_k_l` or `cache_v_l`. This is observation-only and does not
change allocation, graph construction, transport, or execution. The raw traces
directly establish this ownership:

| Worker | Assigned layers | Base cache tensors observed | FP16 allocation | Foreign-layer cache tensors |
|---|---|---:|---:|---:|
| GPU 0 RPC worker | 0 through 24 | 25 K and 25 V | 200 MiB | 0 |
| GPU 1 RPC worker | 25 through 47 | 23 K and 23 V | 184 MiB | 0 |

Each base tensor is FP16 `[1024, 2048, 1, 1]`, 4,194,304 bytes. The named view
and permute tensors in the graph refer to those same resident base tensors and
are not additional cache allocations. Worker 0 contains exactly
`cache_k_l0`/`cache_v_l0` through `cache_k_l24`/`cache_v_l24`. Worker 1 contains
exactly layer 25 through layer 47. Neither worker trace contains a cache tensor
from the other worker's range.

Artifacts are stored in `phase0-results/p05-prefill/`. `comparison.json`
contains the machine-readable correctness and ownership result. The baseline
and RPC directories contain the lossless logits, token IDs, prompt record, and
text logits. `worker0.trace.log` and `worker1.trace.log` contain the direct cache
inventory. `llama-rpc-kv-trace.patch` contains the complete diagnostic patch
against the pinned runtime.

This proves A4 for multi-token prefill and the ownership and residency portion
of A5. It does not yet prove that cache contents remain correct across repeated
decode steps. That is P0.6. P0.5 is complete, and no P0.6 work was performed.

## P0.6 autoregressive generation checkpoint

The trusted direct two-GPU baseline and the two-worker RPC candidate each ran
one warmup followed by five independent measured repetitions. Every repetition
loaded the pinned model from scratch and used the same runtime build, 2,048-token
context, FP16 K/V cache, disabled FlashAttention, layer placement, prompt,
greedy sampler, and seed. Single-turn mode bounded each run.

The prompt `The three primary colors are` produced 34 evaluated tokens after
the runtime's embedded chat formatting. Each run generated 16 tokens. All 12
runs, including warmups, produced this exact token-ID sequence:

```text
785, 2326, 6028, 7987, 304, 279, 2266, 315,
63238, 1894, 26792, 320, 20805, 438, 304, 3100
```

The sequence SHA-256 is
`829afad5a1a7f4e0f7dea10744c45f3505accc4a4a6e2a0a0bbf848bf9a4d522`.
There were no token mismatches, early stops, or failed runs.

Coordinator measurements exclude model load. Time to first token is the
runtime-reported prompt evaluation duration, calculated from its 34-token
prompt throughput. Decode intervals use monotonic timestamps recorded
immediately after each greedy sample. The table reports the median of five run
summaries and the range across all applicable observations:

| Path | Median TTFT | TTFT range | Median decode interval | Interval range | Median generation rate | Rate range |
|---|---:|---:|---:|---:|---:|---:|
| Direct two-GPU baseline | 156.754 ms | 155.251 to 157.993 ms | 52.332 ms | 51.494 to 54.776 ms | 20.4 tokens/s | 20.3 to 20.4 |
| Two-worker loopback RPC | 135.946 ms | 134.974 to 143.824 ms | 56.875 ms | 55.256 to 63.449 ms | 18.8 tokens/s | 18.7 to 18.8 |

The RPC TTFT result is lower for this short prompt, but it is not evidence that
RPC is generally faster. The two paths schedule different backend graphs, and
the result covers one short prompt on loopback only. Steady-state decode is the
more relevant comparison here: RPC is about 7.8 percent slower by median token
rate.

The worker traces contain 75 measured decode executions per stage. Warmups and
prefill are excluded:

| RPC measurement | Median | Range |
|---|---:|---:|
| Worker 0, layers 0 through 24 | 26,077 us | 24,943 to 28,078 us |
| Worker 1, layers 25 through 47 and output | 29,040 us | 28,937 to 29,872 us |
| Worker 0 boundary-get round trip | 26,339 us | 25,135 to 29,304 us |
| Worker 1 final-logits-get round trip | 29,474 us | 29,336 to 31,388 us |

Each steady-state decode crosses one logical FP32 activation of shape
`[5120, 1, 1, 1]`, 20,480 bytes. Because the stock coordinator relays it, that
payload traverses two RPC legs. Each step also returns all 152,064 FP32 logits,
608,256 bytes. The coordinator-worker payload is therefore 649,216 bytes per
decode. Including the observed RPC request and response framing plus two
graph-recompute commands gives 650,215 RPC bytes per decode, excluding TCP/IP
headers. The dominant avoidable cost is the full logits result, not the hidden
activation.

Artifacts are stored in `phase0-results/p06-decode/`. `summary.json` contains
the machine-readable result. The baseline and RPC directories contain each
warmup and measured run. `worker0-graph.log` and `worker1-graph.log` contain all
per-stage graph durations. `diagnostic-trace.patch` contains the observation-only
RPC, KV, and sampler tracing changes against the pinned runtime.

This completes the multi-token decode portion of A5, proves greedy agreement for
A6, and supplies loopback decode evidence for A7. It does not establish LAN
performance or production viability. P0.6 is complete, and no P0.7 work was
performed.

## P0.7 minimal streaming chat-completions endpoint

The pinned upstream `llama-server` was launched as a single-slot coordinator
with local CUDA hidden and the same two GPU-isolated RPC workers. It bound only
to `127.0.0.1:8087` and exposed the model alias
`phase0-qwen2.5-14b-q4_k_m`. Both workers accepted the coordinator connections,
and the request completed through the distributed placement.

`GET /v1/models` returned HTTP 200 with `object: list`, one OpenAI-shaped
`data` entry, the configured model ID, and the expected model metadata including
152,064 vocabulary entries, 2,048 configured context tokens, 14,770,033,664
parameters, and `Q4_K - Medium` type.

The validated request subset for this checkpoint is:

| Field | Supported value in checkpoint |
|---|---|
| `model` | Exact configured alias |
| `messages` | One user message with string content |
| `stream` | `true` |
| `temperature` | `0`, greedy |
| `seed` | Integer |
| `max_tokens` | Positive integer bound |

`POST /v1/chat/completions` returned HTTP 200 with
`Content-Type: text/event-stream`, chunked transfer encoding, and buffering
disabled. The stream contained five JSON events followed by the terminal
`data: [DONE]` sentinel:

1. Assistant role event.
2. Content delta `Red`.
3. Content delta ` Blue`.
4. Content delta ` Green`.
5. Empty delta with `finish_reason: stop` and timing data.

Every JSON event used the same completion ID and model alias. Concatenating the
content deltas produced `Red Blue Green`. The final timing record reported 36
prompt tokens in 174.866 ms and four predicted tokens, including the stop token,
in 175.740 ms, or 22.761 tokens/s. Curl measured 354.412 ms for the complete
HTTP exchange. Its 3.759 ms start-transfer value measures receipt of response
headers and the initial role event, not first generated content.

This is a deliberately narrow Phase 0 compatibility result. It does not claim
support for tools, structured outputs, multimodal content, logprobs, arbitrary
sampling fields, usage-stream options, authentication, TLS, quotas, routing,
or every OpenAI SDK behavior. The upstream server warned that no API key was
configured and CORS allowed all origins. Loopback binding limits exposure for
this experiment, but this configuration is not production-ready.

Artifacts are stored in `phase0-results/p07-api/`. `request.json` is the exact
request. `models.json`, `stream.sse`, and their header files are the raw HTTP
results. `stream.parsed.json` contains the parsed event objects. `server.log`
and the two worker logs record the distributed runtime execution.
`summary.json` contains the machine-readable validation result.

This proves the Phase 0 requirement to stream distributed output through a
minimal chat-completions endpoint. P0.7 is complete, and no P0.8 work was
performed.

## P0.8 cancellation, timeout, backpressure, and fault injection

P0.8 is complete as an investigation, but its acceptance criterion failed.
The stock pinned runtime does not provide bounded, stage-specific distributed
failure semantics. A8 is disproven. The tests used the P0.7 streaming endpoint,
a 512-token maximum response, and one inference slot.

### Client cancellation

Curl disconnected at its configured 350 ms deadline after receiving 1,036
bytes of a valid partial SSE stream. The server detected the disconnect,
cancelled the task, and released the slot 55.274 ms later. It remained healthy.
The partial stream contained neither `[DONE]` nor a success finish reason, so it
did not fabricate completion. It also contained no explicit error event because
the client itself had closed the connection.

### Slow consumer

Curl was limited to 1,024 bytes/s and disconnected at its 2-second deadline
after receiving 2,048 bytes. The server cancelled the task and released the
slot 56.781 ms after detecting the disconnect, then remained healthy. This
proves bounded cleanup after a slow client disconnects. It does not prove a
bounded application-level output queue or producer throttling while a slow
client remains connected. The upstream HTTP stack can buffer into the kernel
socket, and no explicit queue limit was observed for this ordinary stream.

### Worker loss

Each RPC worker was killed during a separate active streaming request:

| Injected failure | Layer ownership | Client closed after injection | Coordinator result | Stage identity in error |
|---|---|---:|---|---|
| Worker 0 killed | Layers 0 through 24 | 1,615 ms | Process abort, exit 139 | No |
| Worker 1 killed | Layers 25 through 47 and output | 1,644 ms | Process abort, exit 139 | No |

Both client connections ended with curl exit code 18, partial transfer. Neither
stream contained `[DONE]` or a success finish reason. The coordinator logged
`Remote RPC server crashed or returned malformed response` and aborted through
`ggml_abort`. The message did not contain the RPC endpoint, worker identity, or
layer range. Loss was bounded only by the kernel detecting the closed socket,
not by a configured runtime deadline. The entire API process was lost instead
of failing only the affected request and placement.

### Worker stalls

Each worker was paused during a separate request. Curl ended both requests at
its configured 2-second client deadline, but this did not interrupt the
coordinator's blocking RPC receive. In both cases:

- the coordinator process remained running but the only inference slot stayed
  blocked;
- `/health` incorrectly continued returning HTTP 200;
- a second inference request could not complete within one second;
- no RPC timeout or stage-specific failure was emitted; and
- progress resumed only after the worker was externally continued.

The source result matches the experiment. The RPC TCP transport calls blocking
`send` and `recv` without socket deadlines. RPC command failures feed
`RPC_STATUS_ASSERT`, whose generic abort omits endpoint and placement identity.
HTTP cancellation cannot interrupt a thread blocked inside that RPC call.

No test produced a fabricated terminal success, which is the one passing fault
property. The runtime nevertheless fails the required bounded and explicit
failure behavior. A production extension would need RPC connect/read/write
deadlines, cancellation-aware transport, endpoint and stage identity in typed
errors, request-scoped failure instead of process abort, placement health tied
to workers, and a documented bounded streaming queue or producer throttle.

Artifacts are stored in `phase0-results/p08-faults/`. Each test retains the
partial SSE data, curl metrics and errors, result record, and relevant server
log. `long-request.json` is the exact request and `summary.json` is the
machine-readable conclusion.

P0.8 is complete with failed acceptance. No P0.9 work was performed.

## P0.9 controlled two-machine LAN reproduction

P0.9 completed a real two-machine reproduction, but it is a partial CPU
validation. The second machine has no NVIDIA GPU, so this result does not
satisfy the Phase 0 two-CUDA-machine exit criterion and does not reproduce
remote CUDA memory pressure or GPU worker behavior.

### Machines and safe transport

The desktop `ergot` used physical GPU 0, a GTX 1080, at `192.168.1.73`. The
laptop `acidito` at `192.168.1.189` is a Dell XPS 13 9370 with an Intel
i7-8550U, four physical cores, eight threads, 15 GiB RAM, 13 GiB initially
available, and no NVIDIA GPU. Both run CachyOS with Linux 7.1.3 and connect to
the same 5 GHz Wi-Fi access point. Both Wi-Fi interfaces use MTU 1500. RDMA is
not available.

The laptop built the pinned llama.cpp revision
`505b1ed15ca80e2a19f12ff4ac365e40fb374053` with its native AVX2 CPU backend
and RPC enabled. The RPC worker bound only to laptop loopback. The desktop
reached it through authenticated SSH local forwarding. No insecure RPC port was
exposed on the LAN, and no third-party relay was used. Measurements therefore
include SSH encryption and forwarding overhead.

Fifty warm-path ICMP samples had zero packet loss, 3.650 ms median RTT, 10.647
ms mean RTT, and a 2.990 to 140.000 ms range. The high maximum confirms
material Wi-Fi jitter. Five 16 MiB SSH stream measurements in each direction
produced:

| Direction | Median throughput | Range |
|---|---:|---:|
| Desktop to laptop | 91.466 Mbit/s | 76.422 to 131.418 Mbit/s |
| Laptop to desktop | 96.550 Mbit/s | 84.628 to 101.008 Mbit/s |

### Placement and correctness

The exact Qwen2.5-14B Q4_K_M model used a `4,1` tensor split selected for the
asymmetric hardware. The desktop GPU owned transformer layers 0 through 39.
The laptop CPU owned layers 40 through 47 and the output head. The desktop
worker inventory contained 40 unique contiguous layer indices and no output
tensor. Its unique model-tensor payload was 6,529,185,816 bytes. The laptop RPC
cache held 1,957,847,040 bytes across 41 large tensor files; smaller tensors are
not cache-eligible under the runtime's 10 MiB threshold.

The first run populated the remote cache and completed in 194.022 seconds,
including load, transfer, prefill, and generation. One warmup and five measured
cached repetitions then ran independently. Their median total process time was
42.680 seconds.

Every run generated the same 16 greedy token IDs as the direct two-GPU P0.6
baseline. The token-sequence SHA-256 remained
`829afad5a1a7f4e0f7dea10744c45f3505accc4a4a6e2a0a0bbf848bf9a4d522`.
There were no correctness mismatches or request failures.

| Metric | Two-machine GPU plus CPU result |
|---|---:|
| Measured repetitions | 5 |
| Median generation rate | 3.3 tokens/s |
| Generation-rate range | 3.0 to 3.4 tokens/s |
| Measured decode intervals | 75 |
| Median decode interval | 316.693 ms |
| Mean decode interval | 328.432 ms |
| Decode-interval range | 288.273 to 512.640 ms |

This is much slower than the 18.8 tokens/s two-local-GPU RPC result. The test
changes both the second-stage backend from CUDA to a mobile CPU and the
transport from loopback to Wi-Fi through SSH, so it does not isolate network
overhead from compute overhead. It does prove that the exact distributed model
can execute correctly across two physical machines over the controlled LAN.

Artifacts are stored in `phase0-results/p09-lan/`. `inventory.txt`, `ping.txt`,
and `ssh-throughput-runs.txt` preserve the machine and network evidence.
`run-0.log` is the cold cache-populating run, and `run-1.log` through
`run-5.log` are the measured cached repetitions. `desktop-gpu-worker.log`
contains the direct layer inventory. `summary.json` contains the
machine-readable conclusion.

P0.9 is complete as a partial CPU LAN validation. The mandatory two-CUDA-machine
criterion remains explicitly unvalidated.

## P0.10 baselines, recommendation, and Phase 0 conclusion

Phase 0 is complete with outcome 2: required production runtime capabilities
are absent or impractical in the validated stock runtime. Distributed execution
is numerically correct and can be fast on two local GPUs, but stock llama.cpp
RPC fails the required security, isolation, cancellation, timeout, health, and
stage-error semantics. The missing second CUDA machine also leaves the mandatory
two-CUDA LAN criterion unvalidated.

### Practical single-machine fallback

The exact Qwen2.5-14B Q4_K_M artifact was benchmarked on one GTX 1080 with 38
transformer layers offloaded to CUDA and the remaining model handled by host
CPU. An attempted all-layer allocation correctly failed because it requested
8,148.38 MiB of CUDA memory, exceeding usable card memory. The explicit
38-layer configuration completed one warmup and five measured repetitions.

All runs produced the same 16 greedy tokens as P0.6 and P0.9. Across the five
measured runs, median generation was 1.9 tokens/s with a 1.7 to 1.9 range. The
75 decode intervals had a 589.645 ms median, 589.128 ms mean, and 404.861 to
875.527 ms range. Median total process time, including model load, was 18.253
seconds. This is the strongest directly comparable single-node fallback on the
available artifact because it preserves the exact model and quantization.

A separate smaller-model download and benchmark was not performed. No claim is
made about its speed. In product use, a smaller model that fits one GPU remains
the preferred practical option when its quality is acceptable.

### Benchmark comparison

| Path | Hardware and transport | Median generation | Range | Median decode interval | Relative to direct two-GPU |
|---|---|---:|---:|---:|---:|
| Direct baseline | Two GTX 1080 GPUs, one process | 20.4 tokens/s | 20.3 to 20.4 | 52.332 ms | 100% |
| Local RPC | Two GPU-isolated workers, loopback | 18.8 tokens/s | 18.7 to 18.8 | 56.875 ms | 92.2% |
| Two-machine RPC | Desktop GPU plus laptop CPU, Wi-Fi and SSH | 3.3 tokens/s | 3.0 to 3.4 | 316.693 ms | 16.2% |
| Single-node fallback | One GTX 1080 plus CPU offload | 1.9 tokens/s | 1.7 to 1.9 | 589.645 ms | 9.3% |

The local RPC result shows that distributed scheduling itself can retain useful
speed when both stages are CUDA and transport is loopback. The LAN result proves
correct two-host execution but cannot isolate network cost because the second
stage changed from a GTX 1080 to a mobile CPU. It must not be presented as a
two-GPU LAN benchmark.

### Assumption disposition

| Assumption | Final result |
|---|---|
| A1, exact contiguous layer ownership | Proven locally and across two hosts |
| A2, stage-only model weights | Proven by per-worker tensor inventories and memory allocation |
| A3, fixed serializable forward boundary | Proven for coordinator-mediated llama.cpp RPC; one logical activation crosses stage ranges |
| A4, multi-token prefill | Proven with exact final logits |
| A5, stage-local KV cache and decode | Proven with all 96 cache tensors and repeated exact generation |
| A6, same-runtime numerical agreement | Proven, bit-identical logits and exact greedy sequences |
| A7, useful performance | Proven on local CUDA loopback; not isolated on a two-CUDA LAN |
| A8, bounded explicit failure behavior | Disproven |

### Runtime decision and maintenance cost

Do not use stock llama.cpp RPC as DLLM's production distributed stage runtime.
Use upstream llama.cpp for supported whole-model execution and as a reference
backend. Proceed with DLLM as a multi-node orchestration, placement, health,
and routing product where each active inference placement fits within one
machine. Do not include distributed layer-stage execution in Phase 1.

The observation-only Phase 0 delta is small, 55 insertions and four deletions
across `ggml-rpc.cpp` and `sampling.cpp`, but it is not a production patch. A
production fork would cross at least these maintenance areas:

- authenticated and encrypted transport, plus resolution of the published RPC
  security advisory;
- connect, read, write, and execution deadlines;
- cancellation that interrupts blocking transport and device work;
- typed request-scoped errors carrying endpoint, stage, and layer identity;
- worker-aware placement health and admission control;
- bounded streaming queues and producer backpressure;
- explicit stage APIs or stable activation framing rather than serialized
  coordinator graphs; and
- result-side sampling or vocabulary reduction to avoid returning 608,256 bytes
  of logits every decode step.

These changes touch unstable internal scheduler, graph, transport, cache, and
HTTP-server interfaces. They constitute a substantial maintained runtime fork,
not a narrow integration patch. Continuous rebasing, security review, backend
compatibility testing, and CUDA 12.9/Pascal build maintenance would be required.
That cost is not justified for Phase 1 by the current evidence.

If distributed stage execution becomes a product requirement later, start a
new feasibility phase with two supported CUDA machines. Re-evaluate vLLM
pipeline parallelism on hardware at its supported compute-capability floor. If
its ownership and deployment semantics still do not fit, define a small custom
stage protocol and executor with failure semantics designed in from the start.
Do not turn the current diagnostic llama.cpp patch into that product fork by
incremental accident.

### Final scope statement

Phase 0 demonstrated a model too large for either local GTX 1080 executing
correctly across two local GPU workers, exact stage-local KV ownership,
streaming API output, reproducible performance, and correct two-physical-machine
CPU-tail execution. It also demonstrated that stock RPC cannot safely survive
worker loss or stalls and that the available second machine cannot validate the
required CUDA LAN configuration.

Artifacts for the final fallback are under
`phase0-results/p10-baselines/single-gpu-offload/`. The consolidated
machine-readable decision is `phase0-results/p10-baselines/summary.json`.
Phase 0 is finished. Phase 1 has not been started.

## Exact local model selection

The local two-GPU checkpoint target is selected. This selection does not apply
automatically to the final two-machine LAN milestone, whose hardware remains
unknown.

| Item | Selection |
|---|---|
| Model | `Qwen/Qwen2.5-14B-Instruct-GGUF` |
| Immutable model revision | `b466e1f8c07172155743e8e1307507d8a4f91fbd` |
| Architecture | Dense Qwen2, 14.7B parameters, 48 transformer layers, 40 query heads, 8 KV heads |
| Quantization | `Q4_K_M` |
| Source | `https://huggingface.co/Qwen/Qwen2.5-14B-Instruct-GGUF` |
| License | Apache-2.0 |
| Runtime | llama.cpp `505b1ed15ca80e2a19f12ff4ac365e40fb374053` |
| Build toolchain | CUDA 12.9.1 container, explicit `CMAKE_CUDA_ARCHITECTURES=61` |
| Initial context | 2,048 tokens, one sequence, FP16 K and V cache |
| Measured layer placement | GPU 0: transformer layers 0 through 24; GPU 1: transformer layers 25 through 47 plus output layer 48 |

Artifact inventory from the Git LFS pointers at the immutable model revision:

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `qwen2.5-14b-instruct-q4_k_m-00001-of-00003.gguf` | 3,991,999,872 | `a09ea5e7b1eafb1b30b241726c3cc3c905c96f14ad41e246ffa5f44e53904f68` |
| `qwen2.5-14b-instruct-q4_k_m-00002-of-00003.gguf` | 3,989,373,504 | `21b9457d079680d284e90ef69607c4b2d8ef64a09d4729cb7b5e1357bdba41ae` |
| `qwen2.5-14b-instruct-q4_k_m-00003-of-00003.gguf` | 1,006,737,120 | `c8d37006760a387a35216e070e6664d7da927f10be8eb870fef2e3d4833d9976` |
| **Total** | **8,988,110,496** | All three files verified successfully on 2026-07-15 |

The total artifact payload is 8.988 GB, or 8.371 GiB. It cannot fit entirely in
either card's observed usable VRAM of 7,420 MiB and 8,151 MiB. All three local
files match their pinned byte sizes and SHA-256 hashes. The measured 1:1 split
uses 4,325.37 MiB on GPU 0 and 4,623.03 MiB on GPU 1 for model, KV, and compute
buffers. The input embedding remains in host memory in the selected llama.cpp
layer mode.

For Qwen2.5-14B, FP16 K and V cache planning uses 48 layers, 8 KV heads, and a
128-element head dimension:

```text
2 tensors * 48 layers * 8 heads * 128 elements * 2 bytes = 196,608 bytes/token
```

At 2,048 tokens this is 384 MiB total. The measured layer allocation produces a
200 MiB KV buffer on worker 0 and a 184 MiB KV buffer on worker 1. A planning
estimate for a single-token boundary hidden state is 5,120
FP32 elements, or 20 KiB before RPC framing. The source probe has not yet
confirmed the actual boundary dtype or whether additional graph tensors cross
the RPC boundary. Prefill traffic scales with the active microbatch and must be
measured rather than inferred from the full context allocation.

The GGUF embeds tokenizer metadata and the chat template, so no separate
tokenizer artifact is selected. The checkpoint will persist exact prompt token
IDs and record the embedded template metadata reported by the pinned runtime.

Q4_K_M was chosen instead of Q3_K_M because it still crosses the single-card
memory boundary with a clear margin while preserving a stronger correctness
baseline. Larger Q5 variants consume unnecessary headroom on the 15.2 GiB
combined usable budget. The 2,048-token context is large enough to exercise
prefill and stage-local KV ownership while reserving several GiB for CUDA
contexts, graph buffers, and transient allocations. Local fit is now measured,
not projected. P0.2 remains open because the separate two-machine inventory and
trusted baseline host are not specified.

## Unresolved decisions and risks

- The local checkpoint hardware is two GTX 1080 GPUs with approximately 15.2
  GiB combined free VRAM at inspection. No second CUDA machine is currently
  available. A later two-CPU-machine LAN run can validate transport behavior
  only and will not satisfy the CUDA two-machine exit criterion.
- The managed host command runner cannot access NVML directly, but approved
  Docker execution with `--gpus all` now exposes both GPUs and supports automated
  Phase 0 runtime probes.
- Installed host CUDA toolkit 13.3 does not compile for Pascal `sm_61`. The
  validated build path is the CUDA 12.9.1 development container pinned above.
- The first CUDA 12.9 container probe was blocked by a stale NVIDIA CDI file
  referencing driver libraries from 580.159.03. The file was regenerated for
  580.173.02 and Docker was restarted. The CUDA 12.9.1 container then detected
  both GPUs and listed `sm_61` compiler support.
- A CUDA 12.9.1 smoke test compiled with `-arch=sm_61`, allocated managed memory,
  and executed a kernel successfully on each GPU. Both devices reported compute
  capability 6.1 and returned the expected result, 42. This proves basic CUDA
  compilation and execution, not inference-runtime feasibility.
- The local two-GPU model is selected. The final two-machine artifact selection
  cannot be confirmed until usable memory on both LAN target machines is known.
- llama.cpp RPC may satisfy practical multi-host inference while failing DLLM's
  independently addressable stage requirement or observability needs.
- The current llama.cpp RPC security status must be resolved at a pinned commit
  before LAN execution.
- vLLM pipeline parallelism may impose homogeneous GPUs, Ray, replicated model
  access, or worker semantics that enlarge the operational and maintenance
  surface.
- Quantization kernels and backend scheduling may make an activation boundary
  inaccessible without a fork.
- The trusted baseline may need CPU offload and could use different kernels,
  complicating tolerance selection.
- Decode sends a boundary activation every token. LAN latency may dominate even
  when memory feasibility succeeds.
- Process loss invalidates its KV-cache range. Phase 0 will fail the request and
  will not imply recovery.

## Reproduction commands

Initial environment inspection:

```sh
uname -a
lscpu
free -h
nvidia-smi
nvcc --version
rustc --version
cargo --version
cmake --version
```

Pinned source and model metadata probes:

```sh
git ls-remote https://github.com/ggml-org/llama.cpp.git HEAD
git ls-remote https://github.com/vllm-project/vllm.git HEAD
git ls-remote https://huggingface.co/Qwen/Qwen2.5-14B-Instruct-GGUF refs/heads/main
```

Corrected llama.cpp configuration and build targets:

```sh
cmake -S . -B build-phase0 -G Ninja \
  -DGGML_CUDA=ON \
  -DGGML_CUDA_FA=OFF \
  -DGGML_RPC=ON \
  -DCMAKE_CUDA_ARCHITECTURES=61 \
  -DCMAKE_BUILD_TYPE=Release
ninja -C build-phase0 ggml-rpc-server llama-cli llama-gguf-split
```

Artifact verification:

```sh
sha256sum qwen2.5-14b-instruct-q4_k_m-0000{1,2,3}-of-00003.gguf
```

Successful bounded RPC probe, with one RPC server per GPU:

```sh
CUDA_VISIBLE_DEVICES=0 GGML_RPC_DEBUG=1 \
  ./ggml-rpc-server --host 127.0.0.1 --port 50052
CUDA_VISIBLE_DEVICES=1 GGML_RPC_DEBUG=1 \
  ./ggml-rpc-server --host 127.0.0.1 --port 50053
CUDA_VISIBLE_DEVICES=-1 ./llama-cli \
  -m qwen2.5-14b-instruct-q4_k_m-00001-of-00003.gguf \
  --rpc 127.0.0.1:50052,127.0.0.1:50053 \
  -ngl all -sm layer -ts 1,1 -fit off -fa off \
  -c 2048 -ctk f16 -ctv f16 \
  -n 1 --temp 0 --seed 1 -p Hello -st \
  --no-display-prompt --simple-io
```

## Recommended next action

Use the two GPU-isolated local containers for P0.4 through P0.8. Persist a fixed
token-ID prompt, capture final-token logits from a same-runtime baseline,
capture the RPC result, inventory remote tensor names rather than only aggregate
buffers, and compare correctness. A later two-CPU-machine direct-LAN run may
provide partial P0.9 transport evidence. Keep RPC restricted to loopback until
its critical security status is resolved. Do not scaffold later product
components or proceed beyond Phase 0.
