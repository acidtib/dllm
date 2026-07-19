# dllm-protocol

Shared types for the DLLM distributed inference network.

## Purpose

Defines the core data structures and validation logic for network state, membership, identity, policy, signed tokens, and transport identity bindings.

## Key Types

- `NetworkState` - Signed network configuration
- `SignedState` - Owner-signed state with signature verification
- `JoinToken` / `SignedJoinToken` - Single-use membership tokens
- `TransportEndpointBinding` - Transport identity bindings
- `HardwareProfile` / `HardwareBenchmark` - Per-node accelerator and measured throughput data used for placement
- `ModelAssignment` / `Placement` - Model-to-node assignment and versioned placement state
- `Member` / `NodeStatus` - Network membership and node health/reachability
- `now_unix()` / `now_ms()` - Time utilities
