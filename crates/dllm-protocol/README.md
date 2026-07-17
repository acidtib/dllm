# dllm-protocol

Shared types for the DLLM distributed inference network.

## Purpose

Defines the core data structures and validation logic for network state, membership, identity, policy, signed tokens, and transport identity bindings.

## Key Types

- `NetworkState` - Signed network configuration
- `SignedState` - Owner-signed state with signature verification
- `JoinToken` / `SignedJoinToken` - Single-use membership tokens
- `TransportEndpointBinding` - Transport identity bindings
- `now_unix()` / `now_ms()` - Time utilities
