# DLLM: Self-Hosted Inference Across Hardware You Control

**Proposal — CLI (`dllm`), daemon (`dllmd`), and Web UI for managing and serving LLMs across user-owned compute**

---

## 1. Executive summary

DLLM is a self-hosted inference platform for managing LLM workloads across machines a user or organization controls: workstations, home GPUs, servers, and CPU-only systems. It presents those resources through a unified, OpenAI-compatible API and provides a CLI and Web UI for network creation, node onboarding, model placement, health, and access control.

DLLM is useful even when a model fits on one machine. It can route models to suitable hardware, manage several inference nodes from one place, expose a consistent API, and show capacity and health across the network. When a model cannot fit on one machine, DLLM's longer-term goal is to execute it across multiple nodes using distributed layer-stage inference.

The defining product decision is that DLLM does not place users into a tool-owned public mesh. Users create and control their own **networks**, decide who may join, choose whether the network is discoverable, assign models, and control who may submit inference requests.

Distributed execution over ordinary networks is the project's primary technical risk. Existing single-node runtimes do not necessarily provide the independently addressable layer stages, distributed KV-cache behavior, batching, and failure semantics DLLM requires. For that reason, the roadmap begins with a **Phase 0 feasibility prototype** before committing to the broader MVP.

---

## 2. Problem statement

Compute capacity is often scattered across several machines, but most self-hosted inference tools still treat each machine as an isolated server. Operating several nodes requires manually choosing where models run, managing separate endpoints, tracking memory and health, and integrating each server independently.

Distributed inference projects address parts of the problem, but tend to focus on research swarms or the mechanics of splitting a model rather than the full operational lifecycle. DLLM is intended to combine:

- user-owned, isolated networks;
- simple node onboarding and revocation;
- model placement and lifecycle management;
- a unified API across heterogeneous hardware;
- optional distributed execution when a model cannot fit on one node;
- clear security and failure semantics; and
- a CLI and Web UI backed by the same management API.

DLLM should not depend on distributed layer execution being faster than single-node inference to be useful. Its orchestration, routing, observability, and access-control features must provide value independently.

---

## 3. Product principles

- **User-owned networks.** Every network has an owner identity and an explicit membership policy. Nothing assumes participation in a global public mesh.
- **Orchestration before aggregation.** DLLM first makes multiple inference machines manageable as one system. Distributed model execution is an additional capability, not the only reason the product exists.
- **Honest performance.** A distributed placement is selected only when it is necessary or measurably beneficial. Adding a slow CPU or WAN node must not automatically place it in the critical path of every request.
- **Private and secure by default.** New networks are private, management interfaces bind locally by default, and remote administration requires explicit authentication.
- **Observable behavior.** Placement, queueing, memory use, network paths, degraded state, and failures are visible rather than hidden behind an apparently healthy API.
- **Measured claims.** Performance and hardware-support claims are based on reproducible benchmarks, not projected capability.

---

## 4. Goals

- **G1 — Unified self-hosted inference management.** Manage several machines and models through one CLI, Web UI, and API surface.
- **G2 — User-owned networks.** Create private networks from a blank slate, onboard and revoke nodes, and configure discoverability, membership, and request access independently.
- **G3 — Hardware-aware placement.** Place whole models, replicas, or distributed stages based on memory, compute, topology, current load, and reliability.
- **G4 — Broad hardware path.** Use a portable baseline runtime where practical, beginning with a narrow, documented hardware target and expanding only after validation.
- **G5 — OpenAI API compatibility.** Support a documented subset of the OpenAI API, beginning with streaming and non-streaming chat completions and model listing.
- **G6 — Distributed dense-model inference.** Allow a model that cannot fit on one participating node to execute across trusted nodes, if the Phase 0 prototype validates the runtime approach.
- **G7 — Full lifecycle UI.** Provide network creation, onboarding, node health, model assignment, placement visibility, API access control, and diagnostics.
- **G8 — Extensible model support.** Use versioned manifests to describe variants of architecture families already implemented by the runtime.

## 5. Non-goals for the initial MVP

- Training or fine-tuning.
- A hosted or managed SaaS offering.
- Payments or incentives for contributed compute.
- Permissionless public compute.
- Tensor parallelism across ordinary LAN or WAN links.
- Distributed expert placement for MoE models.
- Vision, multimodal, or audio input.
- Transparent recovery of an in-flight generation after a stage node disappears.
- Claiming compatibility with every OpenAI endpoint, parameter, or client behavior.

---

## 6. Naming and terminology

| Term | Meaning |
|---|---|
| **DLLM** | The project and product. |
| **`dllmd`** | The daemon running on each participating machine. It manages membership, runtime workers, local state, and APIs. |
| **`dllm`** | The CLI, which talks to the local daemon over a local socket or loopback API. |
| **Network** | A user-owned group of nodes with signed control-plane state and explicit access policies. |
| **Owner node** | The initial control-plane leader for a network. It authorizes membership and publishes signed network state. |
| **Node** | A machine running `dllmd` and participating in one or more networks. |
| **Worker** | A runtime process serving a complete model, model replica, or distributed model stage. |
| **Stage** | A contiguous range of layers assigned to one worker for pipeline execution. |
| **Placement generation** | A versioned, signed description of where a model or its stages are loaded. |
| **Manifest** | Versioned metadata describing a model variant supported by an implemented runtime architecture. |

---

## 7. Architecture overview

Every machine runs the same `dllmd` binary, but nodes may take different roles. The network owner initially acts as the control-plane leader, while inference traffic can flow directly between workers.

```text
Client
  |
  v
OpenAI-compatible API / Request Router
  |
  +---- whole-model worker or replica
  |
  +---- stage 0 -> stage 1 -> ... -> final stage

Control plane
  owner-signed membership, policy, model assignment, and placement state

Data plane
  mutually authenticated encrypted connections between runtime workers
```

### 7.1 Control plane

The first implementation uses an explicit leader rather than distributed consensus:

- The network creator becomes the owner node and control-plane leader.
- Membership, policies, model assignments, and placement generations are signed by the owner identity.
- Member nodes cache and validate the latest signed state.
- The owner can approve, revoke, or rename members and rotate join credentials.
- If the owner is offline, existing placements may continue serving, but control-plane mutations pause.

Leader transfer, replicated ownership, quorum decisions, and owner-key recovery are later hardening features. The proposal does not assume that identical daemon binaries remove the need for coordination.

### 7.2 Data plane

- Encrypted, mutually authenticated node connections, using QUIC-based transport where practical.
- Direct connections are attempted first. When direct connectivity fails, an
  eligible participating `dllmd` node may forward encrypted traffic for other
  members. Running a separate relay service is not a user requirement.
- A node identity is an Ed25519 public key. It authenticates the node but is not itself a routable address; signed endpoint records and discovery are still required.
- Forwarded connections remain end-to-end encrypted, so a forwarding node
  cannot read transport plaintext. Participating inference workers necessarily
  process model state and intermediate tensors.

### 7.3 Local management plane

The CLI and Web UI use the same local management API. It is bound to a local socket or loopback interface by default. Remote management is a distinct mode requiring authenticated, authorized, encrypted access.

---

## 8. Network and policy model

Discoverability, compute membership, and inference access are separate decisions.

```text
Network {
  id: uuid
  name: string
  owner_pubkey: ed25519 public key
  discoverability: "private" | "unlisted" | "listed"
  membership_policy: "invite_only" | "owner_approval"
  request_policy: "local_only" | "api_key"
  state_generation: uint64
  members: [NodeRef]
  model_assignments: [ModelAssignment]
}
```

Permissionless membership is not part of the initial plan. A listed network may be discoverable without allowing arbitrary machines to contribute compute or arbitrary clients to submit requests.

Join credentials must be:

- scoped to one network;
- time-limited or explicitly non-expiring;
- single-use by default;
- revocable and rotatable; and
- safe to paste as a CLI token without exposing the owner's long-term key.

The owner can revoke a member key. Revocation creates a new signed membership generation and triggers placement reconciliation if the node was serving a model.

---

## 9. Workload and placement model

DLLM supports several placement modes rather than assuming every available device belongs in one pipeline:

1. **Whole-model placement** — run a model on one suitable node.
2. **Replica placement** — run several complete copies and route requests based on health, queue depth, or locality.
3. **Dense pipeline placement** — split contiguous layer ranges across trusted nodes when the model cannot fit on one node.
4. **Auxiliary placement** — use other nodes for embeddings, tokenization, draft models, or separate workloads without putting them in the main generation path.

CPU and GPU machines are both resources the scheduler can use, but heterogeneous pipeline placement is not automatically preferred. A CPU stage can bottleneck a GPU pipeline and should be used only when required to fit the model or when measurements predict an acceptable result.

### 9.1 Placement inputs

Node capability includes:

- runtime and accelerator compatibility;
- total and currently available VRAM/RAM;
- measured prefill and decode throughput for the relevant model architecture;
- pairwise latency, bandwidth, and forwarding status;
- current queue depth and KV-cache capacity;
- recent reliability and disconnect history; and
- model weights already cached locally.

Model metadata includes:

- implemented architecture family and runtime version;
- layer and tensor dimensions;
- weight memory per layer;
- activation size per boundary;
- KV-cache cost by context length and concurrency;
- supported quantizations; and
- tokenizer and artifact integrity information.

### 9.2 Placement objective

The scheduler predicts:

- time to first token;
- inter-token latency;
- bottleneck-stage throughput;
- activation transfer, serialization, and forwarding cost;
- memory headroom for weights and KV cache;
- model-loading or migration cost; and
- expected placement stability.

It chooses the simplest placement satisfying the workload. Whole-model placement is preferred when it meets the requirements. Distributed placement is used when necessary or when benchmarks indicate a clear benefit.

Placement changes are versioned. Existing requests remain attached to their placement generation; new requests use the new generation after all required workers report ready.

---

## 10. Model runtime feasibility

The model runtime is the central engineering risk.

`llama.cpp` remains an important portability reference and potential dependency for whole-model workers. However, DLLM must not assume that an unmodified single-node runtime can execute independently addressable layer ranges over a network. Distributed stage execution may require a maintained fork, a custom execution layer, or a new runtime component.

A distributed stage must be able to:

- load only its assigned weights;
- accept and validate intermediate activations;
- execute an arbitrary contiguous layer range;
- maintain the correct per-request KV-cache portion;
- support prompt prefill and autoregressive decode;
- batch compatible work across requests;
- apply backpressure and cancellation; and
- expose deterministic failure and readiness behavior.

Phase 0 exists to determine whether this can be achieved by extending an existing runtime or requires a custom implementation. The result should determine the production runtime choice before the product architecture is locked around it.

### 10.1 Model manifests

Manifests use `schema_version: 1` and describe variants of runtime-supported architecture families. They do not substitute for architecture implementation.

Adding a new size, quantization, or compatible variant may require only a manifest. Adding a new architecture or unsupported operator may require runtime code, tests, and a manifest schema extension.

Each manifest includes artifact sources, hashes or signatures, architecture identifier, tokenizer, tensor metadata, supported runtime versions, quantizations, memory estimates, and compatibility constraints.

### 10.2 MoE models

MoE models may initially run using whole-model or ordinary contiguous layer-stage placement. Distributed expert placement is deferred research.

Expert routing occurs repeatedly across MoE layers, potentially dispatching each token to several experts and returning results at every such layer. Its real cost depends on topology, activation size, batching, expert locality, and routing distribution. DLLM will not claim that expert parallelism minimizes WAN communication until a prototype and benchmarks demonstrate it.

---

## 11. Request scheduling and failure behavior

### 11.1 Admission control

Each placement reports a concurrency limit derived from KV-cache capacity and configured context limits. The entry node maintains a bounded request queue. When the queue is full or a request exceeds its maximum wait time, the API returns a clear rate-limit or service-unavailable response with retry guidance.

Maximum context length, prompt size, output length, concurrent sequences, and per-key quotas are enforceable policy rather than best-effort hints.

### 11.2 Batching

Continuous batching is desirable but depends on the selected runtime supporting batching correctly across distributed stages. It is a Phase 0 measurement target, not an assumed feature inherited automatically from `llama.cpp` or vLLM.

The first feasibility prototype may serve one request at a time. The MVP requires either validated continuous batching or an explicitly documented concurrency limit and queueing model.

### 11.3 Failure semantics

If a required stage disappears during prefill or generation, the initial implementation fails the request explicitly. It does not claim transparent recovery because request-specific KV cache is distributed across the active placement.

The control plane marks the placement degraded, stops admitting new work to it, and attempts to create a replacement placement. Replication, cache reconstruction, checkpointing, and retrying a generation from a known prefix are future reliability work.

---

## 12. API surface

The initial compatibility target is a documented subset of the OpenAI API:

- `GET /v1/models`
- `POST /v1/chat/completions`
- streaming chat completions using SSE

Later endpoints may include:

- `POST /v1/completions`
- `POST /v1/embeddings`

DLLM will publish supported request fields, response fields, error behavior, streaming behavior, and known incompatibilities. “OpenAI-compatible” means clients using that subset can change their base URL; it does not imply universal compatibility with every OpenAI feature or SDK assumption.

A router can expose models from several placements or networks through one endpoint. Ambiguous model names require network-qualified aliases or explicit routing configuration.

---

## 13. CLI and Web UI

### 13.1 Initial CLI

```text
dllm init
dllm status [--json]

dllm network create <name>
dllm network list
dllm network join <token>
dllm network leave <network-id>
dllm network token <network-id> [--expires <duration>]
dllm network revoke-node <network-id> <node-id>

dllm model catalog
dllm model assign <network-id> <model-id> [--quantization <type>]
dllm model unassign <network-id> <model-id>
dllm model status <network-id> <model-id>

dllm node list <network-id>
dllm node capability
dllm serve --network <network-id> --port 8080
dllm ui
```

Discovery and public-listing commands are added only when that service and its abuse controls exist.

### 13.2 Web UI

The first UI includes:

- **Dashboard** — networks, models, health, and degraded placements.
- **Network detail** — members, join credential generation, revocation, and policy.
- **Models** — supported variants, memory estimates, assignment, download, and load status.
- **Placement detail** — whole-model or stage placement, memory, topology, measured latency, queue depth, and placement generation.
- **API access** — keys, scopes, quotas, revocation, and endpoint examples.
- **Diagnostics** — connectivity, direct versus forwarded paths, runtime compatibility, and structured error information.

The bundled UI can be a static application served by `dllmd`. A server-rendered framework is not required merely to call remote APIs. If a future central management service needs server-side sessions, proxying, or multi-user features, that deployment can introduce an authenticated server component separately.

---

## 14. Security model

### 14.1 Protected properties

- Transport connections are encrypted and mutually authenticated.
- Network-state mutations are signed by the current owner identity.
- Management APIs bind locally by default.
- Remote administration requires explicit authentication, authorization, and TLS.
- Model manifests and downloaded artifacts are integrity checked.
- Join credentials and API keys are scoped, revocable, and stored as securely as the platform permits.

### 14.2 Explicit limitations

A participating worker is inside the inference trust boundary. Depending on its stage, a malicious or compromised worker may:

- inspect intermediate activations and request metadata;
- attempt to reconstruct information about prompts or outputs;
- retain the model weights assigned to it;
- return incorrect activations or fabricated capability results;
- delay or selectively fail requests; or
- misreport performance and availability.

Transport encryption does not protect data from the node performing the computation. The initial distributed mode is therefore intended for trusted, user-controlled nodes.

Possible later mitigations include trusted-node labels, pinning boundary layers, redundant verification, signed benchmark receipts, anomaly detection, and replicated execution. None is presented as complete privacy or correctness protection.

### 14.3 Identity storage and recovery

Identity files use restrictive filesystem permissions. The design should support OS keychains or encrypted key storage where available, documented backup, owner-key rotation, and a recovery process. Losing the only owner key must not be quietly treated as recoverable unless a recovery authority was configured in advance.

---

## 15. Observability and benchmarking

The CLI, UI, structured logs, and optional Prometheus endpoint expose:

- model and placement readiness;
- per-stage prefill and decode time;
- time to first token and inter-token latency;
- tokens per second and requests per second;
- activation bytes transferred per token and request;
- direct or forwarded connection paths;
- queue depth, admission rejection, and KV-cache use;
- model download and load progress; and
- node disconnects and placement-generation changes.

Published benchmarks must include:

- exact model and quantization;
- hardware, runtime version, and available memory;
- topology, latency, bandwidth, and forwarding use;
- prompt length, output length, and concurrency;
- time to first token, decode rate, throughput, and failure rate; and
- comparison with the best practical single-node alternative, such as a smaller quantization or CPU offload.

---

## 16. Roadmap

### Phase 0 — Runtime feasibility prototype :DONE

**Purpose:** prove or reject the core distributed-execution assumption before building the full product around it.

- Two controlled Linux machines on the same LAN.
- One documented NVIDIA CUDA hardware baseline.
- One exact dense Qwen model and quantization.
- Fixed, manually configured contiguous layer placement.
- Load only the required weights on each node.
- Execute prompt prefill and autoregressive decode end to end.
- Maintain the correct KV-cache portion per stage.
- Stream output through a minimal chat-completions endpoint.
- Measure activation traffic, time to first token, decode speed, memory use, and failure behavior.
- Determine whether to extend an existing runtime, maintain a fork, or build a custom stage runtime.
- Investigate batching, cancellation, and backpressure; do not assume they work.

**Exit criteria:**

- A model that cannot fit in the available memory of either node completes inference across both.
- Results are correct enough to validate against a trusted single-node implementation.
- Measurements are reproducible and compared with realistic single-node alternatives.
- The team documents the chosen runtime approach and its maintenance cost.
- If the result is not viable, DLLM can continue as a multi-node orchestration and routing product without distributed stage execution.

### Phase 1 — Private-network MVP :DONE

- `dllmd` and `dllm` identity, network creation, invitation, join, leave, revocation, and signed state.
- Owner-led control plane with explicit offline behavior.
- Private, invite-only networks.
- Whole-model placement and the validated dense pipeline mode from Phase 0.
- One supported dense Qwen variant on the documented hardware baseline.
- Streaming and non-streaming chat completions plus model listing.
- Bounded admission queue and explicit saturation errors.
- Minimal UI for onboarding, model assignment, placement, and health.
- Explicit request failure and placement degradation when a required stage disappears.

**MVP success criteria:**

- A user creates a private network, generates a scoped join token, joins a second machine, assigns the validated model, and receives streamed completions without manually configuring peer addresses.
- The owner can revoke the second node and see the placement become unavailable or rebuild safely.
- `dllm status --json` exposes complete network, node, worker, placement, and health state.
- Benchmark results include time to first token, decode rate, network traffic, and comparison with a single-node fallback.

### Phase 2 — Orchestration, replicas, and broader hardware :DONE

- Multiple models and multiple networks per daemon.
- Whole-model replicas and load-aware request routing.
- Hardware benchmark profiles and automatic placement recommendations.
- CPU-only whole-model workers and auxiliary CPU workloads.
- Experimental heterogeneous CPU/GPU pipelines only when measurements justify them.
- Additional dense architecture support, beginning with Gemma.
- Placement preview, compatibility explanations, and capacity planning in the UI.

### Phase 3 — WAN hardening and remote management :DONE

- NAT traversal and relay fallback hardened across supported network environments.
- Authenticated remote management mode with roles and scoped credentials.
- Owner transfer, recovery options, and control-plane backup.
- Placement draining and safer upgrades.
- Fairness policies, quotas, and improved batching based on the chosen runtime.
- Benchmarks across LAN, metro, cross-country, direct, and relayed paths.

### Phase 4 — Discovery and controlled community networks

- Separate listed and unlisted discovery.
- Replace the SSH reverse-tunnel fallback with an embedded, identity-authenticated
  peer transport. Every forwarding and discovery role runs inside `dllmd` on a
  participating node.
- Owner-approval membership workflow.
- Discovery-service hosting, governance, rate limits, moderation, and abuse controls.
- Explicit resource budgets and request-access policy independent of compute membership.
- Public compute remains opt-in and experimental; no payment or incentive layer.

**Exit criteria:**

- A user can install only `dllmd`, join a network, discover peers, authenticate
  with DLLM node identities, and serve inference without deploying a separate
  communication service, configuring SSH, or publicly exposing the runtime.
- Nodes attempt direct connectivity first. If forwarding is necessary, DLLM
  automatically selects an eligible participating node according to signed
  network policy and resource limits.
- Discovery records provide signed reachability information only. Owner-signed
  DLLM state remains authoritative for network membership and authorization.
- Bootstrap, discovery, and forwarding are capabilities of ordinary `dllmd`
  nodes. Normal operation does not require a dedicated or third-party service.
- Status and diagnostics report the discovered endpoint, direct or forwarded
  path, forwarding node, connection failures, and path changes.

### Phase 5 — MoE research

- DeepSeek and Qwen MoE architecture support in the runtime.
- Whole-model or contiguous layer-stage serving first.
- Prototype distributed expert placement and measure repeated per-layer dispatch cost.
- Explore expert replication, locality-aware routing, batching, and failure recovery.
- Ship expert-parallel placement only if it demonstrates useful performance on documented topologies.

### Phase 6 — Multimodal exploration

- Versioned manifest extensions for preprocessing, encoders, and projectors.
- Dedicated encoder-stage placement.
- Image inputs through the supported chat-completions subset.
- Initial targets selected based on runtime maturity rather than promised in advance.

---

## 17. Risks and open decisions

### Primary risks

- **Runtime feasibility:** arbitrary layer-stage execution, distributed KV cache, and cross-stage batching may require a substantial maintained runtime fork or custom engine.
- **Distributed latency:** a model may fit across machines but still be too slow for interactive use.
- **Heterogeneous bottlenecks:** adding slower nodes can reduce throughput and increase latency.
- **Failure recovery:** losing a stage usually invalidates in-flight KV-cache state.
- **Worker trust:** participating nodes can inspect intermediate state, retain assigned weights, misreport capability, or corrupt results.
- **Backend fragmentation:** different runtimes may not share compatible stage, cache, batching, or quantization semantics.
- **Control-plane availability:** an owner-led design is simple but temporarily prevents mutations when the owner is offline.
- **Discovery abuse:** listed networks require network-wide rate limits,
  moderation, and governance even when discovery remains peer-to-peer.

### Decisions to resolve during Phase 0

1. Extend `llama.cpp`, maintain a purpose-built fork, or implement a custom stage runtime?
2. Which exact Qwen model, quantization, CUDA version, and minimum GPU define the first reproducible target?
3. Can stage-local batching preserve correctness and provide useful throughput?
4. What activation encoding and transport framing give acceptable overhead?
5. Is distributed inference competitive with a smaller single-node quantization or CPU offload for the intended user?
6. What parts of the product remain valuable if distributed execution is technically correct but too slow over WAN links?

---

## 18. Initial tech stack

### Repository structure

DLLM will be developed as a monorepo containing the daemon, CLI, shared Rust crates, Web UI, model manifests, protocol definitions, tests, benchmarks, and documentation.

### Initial structure
```
dllm/
├── Cargo.toml
├── crates/
│   ├── dllm-cli/
│   ├── dllmd/
│   ├── dllm-control/
│   ├── dllm-protocol/
│   ├── dllm-runtime/
│   └── dllm-transport/
├── web/
├── manifests/
├── benchmarks/
├── integration-tests/
└── docs/
```

The exact crate boundaries may evolve during Phase 0. Avoid prematurely splitting experimental runtime code into many abstractions before the distributed execution approach is validated.

The monorepo should provide:

- one versioned source of truth for protocol and state types;
- atomic changes across the daemon, CLI, UI, manifests, and documentation;
- shared formatting, linting, testing, and release automation;
- reproducible integration tests across multiple local daemon processes; and
- independently buildable artifacts for dllm, dllmd, and the Web UI.

| Component | Initial choice | Rationale and constraint |
|---|---|---|
| **Daemon, control plane, scheduler, transport** | **Rust** | Static distribution, memory safety, asynchronous networking, and strong QUIC ecosystem. Candidate libraries include Tokio, Axum, Quinn, and evaluated iroh components. |
| **CLI** | **Rust with `clap`** | Shared types and release process with the daemon. |
| **Distributed model runtime** | **To be selected by Phase 0** | Must be chosen through implementation evidence. Stock `llama.cpp` support is not assumed. |
| **Whole-model portable runtime** | **`llama.cpp` candidate** | Broad GGUF and hardware support, subject to integration and licensing review. |
| **High-throughput whole-model runtime** | **vLLM candidate, later** | Useful on compatible accelerators, but not a universal fallback and not assumed to provide DLLM's distributed stage semantics. |
| **Bundled Web UI** | **Static TypeScript SPA** | Served locally by `dllmd`; avoids requiring a separate server runtime. A server component can be added for future remote multi-user management. |
| **Model manifests** | **YAML, versioned** | Human-readable metadata for implemented architecture variants. |
| **Node configuration** | **TOML** | Simple local configuration with explicit schema migration. Secrets should use safer platform storage where possible. |
| **Persistent state** | **SQLite or embedded transactional store** | Needed for signed generations, membership, assignments, credentials, and migrations; exact choice remains open. |

---

## 19. Prior art to study

DLLM should be built around its own product and trust model, while validating its design against prior work:

- **mesh-llm** — distributed inference shape, P2P transport choices, API behavior, and any demonstrated pipeline or expert-placement tradeoffs.
- **Petals and Hivemind** — pipeline inference over unreliable networks, block placement, and behavior under membership churn.
- **Shard** — threat-model documentation and WAN benchmark methodology.
- **llama.cpp** — portable inference, GGUF, quantization, KV-cache behavior, and the scope of changes required for remote layer stages.
- **vLLM** — batching, admission control, memory management, and throughput behavior on supported hardware.

All comparisons and capability claims should be verified against the versions evaluated during implementation.

---

## 20. Summary

DLLM's near-term product is a self-hosted control plane and unified API for inference hardware a user owns. Its most ambitious capability—executing one model across ordinary networked machines—is treated as a hypothesis to validate, not a solved dependency.

The project succeeds at the prototype stage by proving distributed dense-model inference with honest measurements. It succeeds as an MVP by making a private two-node network safe and easy to operate. It succeeds long term by expanding placement choices only where benchmarks, failure behavior, and security boundaries justify them.
