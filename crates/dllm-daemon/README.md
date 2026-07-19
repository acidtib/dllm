# dllmd

Node daemon for the DLLM distributed inference network.

## Purpose

Runs the core service that manages inference, credentials, network store, and peer transport.

## Features

- HTTP API server (axum)
- Credential and inference registry
- Network state persistence
- Peer transport integration
- Onboarding state machine (`Inactive` -> `Joining` -> `Active`/`Failed`) gating the API until a node has joined a network
- Hardware auto-benchmark: measures achievable `gpu_layers`/`context_size` per model and backend, caches the result, and publishes it as a signed `HardwareBenchmark`
