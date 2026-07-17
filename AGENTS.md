## Writing style

- Do not use emojis anywhere: code, comments, commit messages, or chat replies.
- Do not use em-dashes. Use commas, colons, parentheses, or separate sentences.
- Avoid filler "LLM-tell" phrasing. Write plainly and directly.

## Code comments

- Comment to explain why something is done or to flag a non-obvious constraint.
- Do not write summary comments that just restate what the next line does.
- Skip section-header and narration comments. Let the code speak for itself.

## Git

- Never add a co-author trailer to commits (no "Co-Authored-By" line).
- Keep commit messages short and factual.
- Preserve unrelated and user-owned working-tree changes.

## Project architecture

- `dllmd` is the only service users must install and operate.
- Do not introduce a dedicated relay, discovery, bootstrap, or coordination
  service as a requirement.
- Ordinary participating `dllmd` nodes provide discovery, NAT traversal, and
  encrypted forwarding roles when eligible.
- Use rust-libp2p 0.56 for the Phase 4 embedded peer network unless a documented
  milestone explicitly changes that decision.
- SSH may administer test machines, but DLLM peer traffic must not use SSH or
  require SSH configuration.
- Discovery records publish reachability only. They do not grant membership,
  inference access, forwarding eligibility, or any other authorization.
- Owner-signed DLLM state is the authority for membership, policy, transport
  identity bindings, rotation, and revocation.
- Keep DLLM node identity keys separate from libp2p transport identity keys.

## Workspace crates

```
crates/dllm-protocol  — Shared types: network state, membership, identity,
                          policy, signed tokens, transport identity bindings
crates/dllm-daemon      — Node daemon: API server, credentials, inference
                          registry, network store (binary name: dllmd)
crates/dllm-cli        — CLI client (binary name: dllm)
crates/dllm-runtime    — Inference runtime: manages llama.cpp child processes
crates/dllm-transport  — libp2p peer transport layer
```

## Key directories

```
docs/        — Phase plans and milestone evidence
manifests/   — Model manifest files (GGUF quantization specs)
scripts/     — Container and helper scripts
web/         — Web UI
```

## Phase workflow

- Each phase has an engineering log at `docs/PHASE<N>.md` with acceptance
  criteria and a milestone checklist. Use the current phase's log as the
  active implementation sequence.
- A milestone is complete only after its implementation, automated coverage,
  applicable physical validation, diagnostics, evidence, and cleanup pass.
- Store structured milestone evidence under
  `docs/results/phase<N>-results/<milestone>/summary.json`.
- Physical validation may use SSH for deployment, administration, inspection,
  and cleanup only.
- Remove remote test services, binaries, temporary state, keys, firewall rules,
  and listeners after validation.

## Validation

Run these checks before completing a milestone:

```sh
cargo fmt --all
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check
```

Build and run:

```sh
cargo build --release
cargo run --release --bin dllmd -- --help
```
