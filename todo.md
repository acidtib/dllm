# Phase 3 VPS Handoff

Resume Phase 3 from `docs/PHASE3.md`. The current machine-readable decision is
`phase3-results/final-summary.json`. Phase 4 has not started.

## VPS access to provide

Two Linux x86-64 VPS hosts are needed:

- one geographically near the local DLLM machines for the metro profile;
- one cross-country, preferably at least 1,500 km away;
- at least 2 vCPU and 2 GiB RAM each;
- public IPv4 connectivity;
- SSH key access using connection strings such as `user@host`;
- one TCP port permitted through the provider and host firewalls; and
- Docker if convenient, though it is not required.

Record tomorrow:

- Metro VPS SSH: `root@165.245.194.22`
- Metro VPS location: `Kansas, US`
- Cross-country VPS SSH: `root@161.35.1.120`
- Cross-country VPS location: `New York, US`
- Existing SSH key authorized: `yes`
- Passwordless sudo available: `yes, root login`
- TCP ports allowed by each provider firewall: `7443 and 7444 during validation`

Do not put passwords, private SSH keys, API tokens, or VPS provider credentials
in this file.

## Remaining Phase 3 work

- Complete P3.2 with an automatic outbound reverse tunnel or equivalent NAT
  traversal path through the relay.
- Deploy the authenticated `dllm-relay` path to the VPS hosts.
- Run the same workload on physical metro and cross-country direct paths.
- Run the same workload on physical metro and cross-country relayed paths.
- Record latency, time to first byte, total time, throughput, request and
  response traffic, failures, and recovery timing.
- Test direct-path loss and automatic relay fallback.
- Test relay interruption and recovery without exposing local runtime ports.
- Update `phase3-results/p36-network-matrix/summary.json` with physical evidence.
- Complete P3.2, P3.6, and P3.7 acceptance checks in `docs/PHASE3.md`.
- Change `phase3-results/final-summary.json` to complete only when every Phase 3
  acceptance criterion passes.
- Run `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings`.
- Stop remote and local test services, verify test ports are closed, and commit
  the final Phase 3 results.

## Current state

Completed milestones: P3.0, P3.1, P3.3, P3.4, and P3.5.

Open milestones: P3.2, P3.6, and P3.7.

Relevant commits:

- `1fe3b2a Harden peer relay transport`
- `43e0ec3 Add recovery draining and quotas`
- `eb6aa1f Add authenticated relay fallback`
- `affce00 Complete scoped credential management`
- `d446e62 Start Phase 3 authorization`

Current local and emulated evidence:

- `phase3-results/p35-batching/summary.json`
- `phase3-results/p36-network-matrix/summary.json`
- `phase3-results/final-summary.json`
