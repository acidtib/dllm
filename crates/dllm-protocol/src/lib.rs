use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub const LEGACY_SCHEMA_VERSION: u32 = 1;
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkState {
    pub schema_version: u32,
    pub network_id: Uuid,
    pub name: String,
    #[serde(rename = "authority_pubkey", alias = "owner_pubkey")]
    pub owner_pubkey: [u8; 32],
    pub generation: u64,
    pub members: Vec<Member>,
    pub model_assignments: Vec<ModelAssignment>,
    pub placements: Vec<Placement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hardware_profiles: Vec<HardwareProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transport_bindings: Vec<TransportEndpointBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transport_revocations: Vec<TransportEndpointRevocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forwarding_policy: Vec<ForwardingPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_budgets: Vec<ResourceBudget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub banned: Vec<MembershipBan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardingPolicy {
    pub node_pubkey: [u8; 32],
    pub max_reservations: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceBudget {
    pub node_pubkey: [u8; 32],
    pub max_in_flight: u32,
    pub max_requests_per_window: u32,
    pub window_seconds: u32,
    pub granted_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MembershipBan {
    pub node_pubkey: [u8; 32],
    pub banned_at_unix: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessRequest {
    pub node_pubkey: [u8; 32],
    pub requested_endpoint: String,
    pub note: String,
    pub requested_at_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_peer_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedAccessRequest {
    pub request: AccessRequest,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateFetchRequest {
    pub node_pubkey: [u8; 32],
    pub requested_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedStateFetchRequest {
    pub request: StateFetchRequest,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbuseReport {
    pub reporter_pubkey: [u8; 32],
    pub subject_pubkey: [u8; 32],
    pub category: String,
    pub note: String,
    pub reported_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportEndpointBinding {
    pub node_pubkey: [u8; 32],
    pub transport_peer_id: String,
    pub binding_generation: u64,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportEndpointRevocation {
    pub node_pubkey: [u8; 32],
    pub transport_peer_id: String,
    pub binding_generation: u64,
    pub revoked_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareProfile {
    pub node_pubkey: [u8; 32],
    pub observed_at_unix: u64,
    pub cpu: CpuCapability,
    pub system_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub accelerators: Vec<AcceleratorCapability>,
    pub runtimes: Vec<RuntimeCapability>,
    pub benchmarks: Vec<HardwareBenchmark>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CpuCapability {
    pub model: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcceleratorCapability {
    pub backend: String,
    pub device_name: String,
    pub device_id: String,
    pub driver: String,
    pub memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeCapability {
    pub runtime: String,
    pub revision: String,
    pub backend: String,
    pub architectures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HardwareBenchmark {
    pub model: String,
    pub backend: String,
    #[serde(default)]
    pub gpu_layers: u32,
    pub context_size: u32,
    pub concurrency: u32,
    pub prompt_tokens_per_second_milli: u64,
    pub decode_tokens_per_second_milli: u64,
    pub peak_memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAssignment {
    pub model: String,
    pub node_pubkey: [u8; 32],
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Placement {
    pub placement_id: Uuid,
    pub model: String,
    pub node_pubkey: [u8; 32],
    pub created_generation: u64,
    #[serde(default)]
    pub lifecycle: PlacementLifecycle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlacementLifecycle {
    #[default]
    Ready,
    Draining,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub node_pubkey: [u8; 32],
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_endpoint: Option<String>,
    pub joined_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ready,
    Unknown,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Local,
    Direct,
    Relay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeStatus {
    pub node_pubkey: [u8; 32],
    pub endpoint: String,
    #[serde(rename = "authority", alias = "owner")]
    pub owner: bool,
    pub health: HealthState,
    pub transport: Option<TransportKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerStatus {
    pub worker_id: Uuid,
    pub node_pubkey: [u8; 32],
    pub model: String,
    pub health: HealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlacementStatus {
    pub placement_id: Uuid,
    pub model: String,
    pub generation: u64,
    pub worker_ids: Vec<Uuid>,
    pub health: HealthState,
    pub lifecycle: PlacementLifecycle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagementStatus {
    pub network: SignedState,
    pub nodes: Vec<NodeStatus>,
    pub workers: Vec<WorkerStatus>,
    pub placements: Vec<PlacementStatus>,
    pub health: HealthState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedState {
    pub state: NetworkState,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateFetchResponse {
    pub state: SignedState,
    pub bootstrap_multiaddrs: Vec<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("state authority key does not match signer")]
    OwnerMismatch,
    #[error("invalid state signature")]
    InvalidSignature,
    #[error("generation must be non-zero")]
    InvalidGeneration,
    #[error("transport binding generation must be non-zero")]
    InvalidBindingGeneration,
    #[error("transport binding endpoint identity is not a valid libp2p peer ID")]
    InvalidTransportPeerId,
    #[error("transport binding expiry must be later than its issue time")]
    InvalidBindingLifetime,
    #[error("transport binding refers to a node outside the network")]
    BindingNodeUnknown,
    #[error("multiple active transport bindings exist for one node")]
    DuplicateNodeBinding,
    #[error("transport endpoint identity is bound to multiple nodes")]
    DuplicateTransportPeerId,
    #[error("a revoked or stale transport binding is active")]
    RevokedTransportBinding,
    #[error("forwarding policy refers to a node outside the network")]
    ForwardingNodeUnknown,
    #[error("forwarding policy contains a duplicate node")]
    DuplicateForwardingNode,
    #[error("forwarding reservation limit must be non-zero")]
    InvalidForwardingLimit,
    #[error("state generation {supplied} does not advance current generation {current}")]
    StaleStateGeneration { supplied: u64, current: u64 },
    #[error("resource budget refers to a node outside the network")]
    BudgetNodeUnknown,
    #[error("resource budget contains a duplicate node")]
    DuplicateBudgetNode,
    #[error("resource budget must allow at least one request")]
    EmptyResourceBudget,
    #[error("ban target is an active member; revoke membership first")]
    BanTargetIsMember,
}

impl SignedState {
    pub fn sign(mut state: NetworkState, key: &SigningKey) -> Result<Self, StateError> {
        state.schema_version = SCHEMA_VERSION;
        validate_state(&state, key.verifying_key().as_bytes())?;
        let bytes = serde_json::to_vec(&state).expect("protocol state is serializable");
        let signature = key.sign(&bytes).to_bytes().to_vec();
        Ok(Self { state, signature })
    }

    pub fn verify(&self) -> Result<(), StateError> {
        validate_state(&self.state, &self.state.owner_pubkey)?;
        let key = VerifyingKey::from_bytes(&self.state.owner_pubkey)
            .map_err(|_| StateError::InvalidSignature)?;
        let bytes = state_signing_bytes(&self.state);
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| StateError::InvalidSignature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| StateError::InvalidSignature)
    }

    pub fn verify_newer_than(&self, current_generation: u64) -> Result<(), StateError> {
        self.verify()?;
        if self.state.generation <= current_generation {
            return Err(StateError::StaleStateGeneration {
                supplied: self.state.generation,
                current: current_generation,
            });
        }
        Ok(())
    }
}

fn validate_state(state: &NetworkState, signer: &[u8; 32]) -> Result<(), StateError> {
    if state.schema_version != SCHEMA_VERSION && state.schema_version != LEGACY_SCHEMA_VERSION {
        return Err(StateError::SchemaVersion(state.schema_version));
    }
    if state.generation == 0 {
        return Err(StateError::InvalidGeneration);
    }
    if &state.owner_pubkey != signer {
        return Err(StateError::OwnerMismatch);
    }
    let mut bound_nodes = std::collections::HashSet::new();
    let mut bound_peers = std::collections::HashSet::new();
    for binding in &state.transport_bindings {
        if binding.binding_generation == 0 {
            return Err(StateError::InvalidBindingGeneration);
        }
        if binding
            .transport_peer_id
            .parse::<libp2p_identity::PeerId>()
            .is_err()
        {
            return Err(StateError::InvalidTransportPeerId);
        }
        if binding.expires_at_unix <= binding.issued_at_unix {
            return Err(StateError::InvalidBindingLifetime);
        }
        let known = binding.node_pubkey == state.owner_pubkey
            || state
                .members
                .iter()
                .any(|member| member.node_pubkey == binding.node_pubkey);
        if !known {
            return Err(StateError::BindingNodeUnknown);
        }
        if !bound_nodes.insert(binding.node_pubkey) {
            return Err(StateError::DuplicateNodeBinding);
        }
        if !bound_peers.insert(&binding.transport_peer_id) {
            return Err(StateError::DuplicateTransportPeerId);
        }
        if state.transport_revocations.iter().any(|revocation| {
            revocation.transport_peer_id == binding.transport_peer_id
                || (revocation.node_pubkey == binding.node_pubkey
                    && revocation.binding_generation >= binding.binding_generation)
        }) {
            return Err(StateError::RevokedTransportBinding);
        }
    }
    for revocation in &state.transport_revocations {
        if revocation.binding_generation == 0 {
            return Err(StateError::InvalidBindingGeneration);
        }
        if revocation
            .transport_peer_id
            .parse::<libp2p_identity::PeerId>()
            .is_err()
        {
            return Err(StateError::InvalidTransportPeerId);
        }
    }
    let mut forwarding_nodes = std::collections::HashSet::new();
    for policy in &state.forwarding_policy {
        let known = policy.node_pubkey == state.owner_pubkey
            || state
                .members
                .iter()
                .any(|member| member.node_pubkey == policy.node_pubkey);
        if !known {
            return Err(StateError::ForwardingNodeUnknown);
        }
        if !forwarding_nodes.insert(policy.node_pubkey) {
            return Err(StateError::DuplicateForwardingNode);
        }
        if policy.max_reservations == 0 {
            return Err(StateError::InvalidForwardingLimit);
        }
    }
    let mut budget_nodes = std::collections::HashSet::new();
    for budget in &state.resource_budgets {
        let known = budget.node_pubkey == state.owner_pubkey
            || state
                .members
                .iter()
                .any(|member| member.node_pubkey == budget.node_pubkey);
        if !known {
            return Err(StateError::BudgetNodeUnknown);
        }
        if !budget_nodes.insert(budget.node_pubkey) {
            return Err(StateError::DuplicateBudgetNode);
        }
        if budget.max_in_flight == 0 && budget.max_requests_per_window == 0 {
            return Err(StateError::EmptyResourceBudget);
        }
    }
    for ban in &state.banned {
        if ban.node_pubkey == state.owner_pubkey
            || state
                .members
                .iter()
                .any(|member| member.node_pubkey == ban.node_pubkey)
        {
            return Err(StateError::BanTargetIsMember);
        }
    }
    Ok(())
}

fn state_signing_bytes(state: &NetworkState) -> Vec<u8> {
    if state.schema_version != LEGACY_SCHEMA_VERSION {
        return serde_json::to_vec(state).expect("protocol state is serializable");
    }
    let mut value = serde_json::to_value(state).expect("protocol state is serializable");
    let object = value
        .as_object_mut()
        .expect("network state serializes as an object");
    if let Some(authority) = object.remove("authority_pubkey") {
        object.insert("owner_pubkey".into(), authority);
    }
    serde_json::to_vec(&value).expect("legacy protocol state is serializable")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinToken {
    pub schema_version: u32,
    pub network_id: Uuid,
    pub token_id: Uuid,
    pub owner_pubkey: [u8; 32],
    pub owner_endpoint: String,
    pub expires_at_unix: Option<u64>,
    pub single_use: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedJoinToken {
    pub token: JoinToken,
    pub signature: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("join token must be single-use")]
    NotSingleUse,
    #[error("invalid join token signature")]
    InvalidSignature,
    #[error("join token expired at {0}")]
    Expired(u64),
}

impl JoinToken {
    pub fn new(
        network_id: Uuid,
        owner_pubkey: [u8; 32],
        owner_endpoint: String,
        expires_at_unix: Option<u64>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            network_id,
            token_id: Uuid::new_v4(),
            owner_pubkey,
            owner_endpoint,
            expires_at_unix,
            single_use: true,
        }
    }
}

impl SignedJoinToken {
    pub fn issue(
        network_id: Uuid,
        owner_key: &SigningKey,
        owner_endpoint: String,
        expires_at_unix: Option<u64>,
    ) -> Self {
        let token = JoinToken::new(
            network_id,
            owner_key.verifying_key().to_bytes(),
            owner_endpoint,
            expires_at_unix,
        );
        let bytes = serde_json::to_vec(&token).expect("join token is serializable");
        let signature = owner_key.sign(&bytes).to_bytes().to_vec();
        Self { token, signature }
    }

    pub fn verify(&self, now_unix: u64) -> Result<(), TokenError> {
        if self.token.schema_version != SCHEMA_VERSION {
            return Err(TokenError::SchemaVersion(self.token.schema_version));
        }
        if !self.token.single_use {
            return Err(TokenError::NotSingleUse);
        }
        if let Some(expires_at) = self.token.expires_at_unix {
            if now_unix >= expires_at {
                return Err(TokenError::Expired(expires_at));
            }
        }
        let key = VerifyingKey::from_bytes(&self.token.owner_pubkey)
            .map_err(|_| TokenError::InvalidSignature)?;
        let bytes = serde_json::to_vec(&self.token).expect("join token is serializable");
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| TokenError::InvalidSignature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| TokenError::InvalidSignature)
    }
}

impl SignedAccessRequest {
    pub fn sign(request: AccessRequest, key: &SigningKey) -> Self {
        let bytes = serde_json::to_vec(&request).expect("access request is serializable");
        let signature = key.sign(&bytes).to_bytes().to_vec();
        Self { request, signature }
    }

    pub fn verify(&self) -> Result<(), TokenError> {
        let key = VerifyingKey::from_bytes(&self.request.node_pubkey)
            .map_err(|_| TokenError::InvalidSignature)?;
        let bytes = serde_json::to_vec(&self.request).expect("access request is serializable");
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| TokenError::InvalidSignature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| TokenError::InvalidSignature)
    }
}

const STATE_FETCH_SIGNATURE_DOMAIN: &[u8] = b"dllm-state-fetch-v1";

impl SignedStateFetchRequest {
    pub fn sign(request: StateFetchRequest, key: &SigningKey) -> Self {
        let bytes = state_fetch_signing_bytes(&request);
        let signature = key.sign(&bytes).to_bytes().to_vec();
        Self { request, signature }
    }

    pub fn verify(&self) -> Result<(), TokenError> {
        let key = VerifyingKey::from_bytes(&self.request.node_pubkey)
            .map_err(|_| TokenError::InvalidSignature)?;
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| TokenError::InvalidSignature)?;
        key.verify(&state_fetch_signing_bytes(&self.request), &signature)
            .map_err(|_| TokenError::InvalidSignature)
    }
}

fn state_fetch_signing_bytes(request: &StateFetchRequest) -> Vec<u8> {
    let request = serde_json::to_vec(request).expect("state fetch request is serializable");
    let mut bytes = Vec::with_capacity(STATE_FETCH_SIGNATURE_DOMAIN.len() + request.len());
    bytes.extend_from_slice(STATE_FETCH_SIGNATURE_DOMAIN);
    bytes.extend_from_slice(&request);
    bytes
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(key: &SigningKey) -> NetworkState {
        NetworkState {
            schema_version: SCHEMA_VERSION,
            network_id: Uuid::new_v4(),
            name: "private".into(),
            owner_pubkey: key.verifying_key().to_bytes(),
            generation: 1,
            members: vec![],
            model_assignments: vec![],
            placements: vec![],
            hardware_profiles: vec![],
            transport_bindings: vec![],
            transport_revocations: vec![],
            forwarding_policy: vec![],
            resource_budgets: vec![],
            banned: vec![],
        }
    }

    #[test]
    fn signed_state_verifies_and_detects_tampering() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut signed = SignedState::sign(state(&key), &key).unwrap();
        assert_eq!(signed.verify(), Ok(()));
        signed.state.name = "tampered".into();
        assert_eq!(signed.verify(), Err(StateError::InvalidSignature));
    }

    #[test]
    fn legacy_owner_named_state_still_verifies() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut legacy = state(&key);
        legacy.schema_version = LEGACY_SCHEMA_VERSION;
        let signature = key.sign(&state_signing_bytes(&legacy)).to_bytes().to_vec();
        let serialized = serde_json::json!({
            "state": serde_json::from_slice::<serde_json::Value>(&state_signing_bytes(&legacy)).unwrap(),
            "signature": signature,
        });
        assert!(serialized["state"].get("owner_pubkey").is_some());
        let signed: SignedState = serde_json::from_value(serialized).unwrap();
        assert_eq!(signed.verify(), Ok(()));
    }

    #[test]
    fn signer_must_be_network_owner() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let other = SigningKey::generate(&mut rand::thread_rng());
        assert_eq!(
            SignedState::sign(state(&key), &other),
            Err(StateError::OwnerMismatch)
        );
    }

    #[test]
    fn join_tokens_are_scoped_and_single_use() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let token = SignedJoinToken::issue(
            Uuid::new_v4(),
            &key,
            "http://127.0.0.1:7337".into(),
            Some(100),
        );
        assert_eq!(token.token.schema_version, SCHEMA_VERSION);
        assert!(token.token.single_use);
        assert_eq!(token.token.owner_pubkey, key.verifying_key().to_bytes());
        assert_eq!(token.verify(99), Ok(()));
        assert_eq!(token.verify(100), Err(TokenError::Expired(100)));
    }

    #[test]
    fn signed_join_token_detects_tampering() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut token =
            SignedJoinToken::issue(Uuid::new_v4(), &key, "http://127.0.0.1:7337".into(), None);
        token.token.network_id = Uuid::new_v4();
        assert_eq!(token.verify(0), Err(TokenError::InvalidSignature));
    }

    #[test]
    fn signed_state_replay_does_not_replace_a_newer_generation() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let signed = SignedState::sign(state(&key), &key).unwrap();
        assert_eq!(
            signed.verify_newer_than(1),
            Err(StateError::StaleStateGeneration {
                supplied: 1,
                current: 1
            })
        );
    }

    #[test]
    fn signed_state_rejects_a_malformed_transport_peer_id() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        state.transport_bindings.push(TransportEndpointBinding {
            node_pubkey: state.owner_pubkey,
            transport_peer_id: "not-a-peer-id".into(),
            binding_generation: 1,
            issued_at_unix: 1,
            expires_at_unix: 2,
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::InvalidTransportPeerId)
        );
    }

    #[test]
    fn signed_access_request_verifies_and_detects_tampering() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let request = AccessRequest {
            node_pubkey: key.verifying_key().to_bytes(),
            requested_endpoint: "http://127.0.0.1:7337".into(),
            note: "please let me in".into(),
            requested_at_unix: 100,
            transport_peer_id: None,
        };
        let signed = SignedAccessRequest::sign(request.clone(), &key);
        assert!(signed.verify().is_ok());
        // Tamper with the note.
        let mut tampered = request;
        tampered.note = "evil".into();
        let bad = SignedAccessRequest {
            request: tampered,
            signature: signed.signature.clone(),
        };
        assert!(bad.verify().is_err());
    }

    #[test]
    fn signed_access_request_wrong_key_rejected() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let other = SigningKey::generate(&mut rand::thread_rng());
        let request = AccessRequest {
            node_pubkey: other.verifying_key().to_bytes(),
            requested_endpoint: "http://127.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 100,
            transport_peer_id: None,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        // Signature was made with `key` but node_pubkey is `other`.
        assert!(signed.verify().is_err());
    }

    #[test]
    fn old_access_request_deserializes_without_transport_identity() {
        let value = serde_json::json!({
            "node_pubkey": vec![1; 32],
            "requested_endpoint": "http://127.0.0.1:7337",
            "note": "legacy",
            "requested_at_unix": 100
        });
        let request: AccessRequest = serde_json::from_value(value).unwrap();
        assert_eq!(request.transport_peer_id, None);
    }

    #[test]
    fn signed_state_fetch_request_verifies_and_detects_tampering() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let request = StateFetchRequest {
            node_pubkey: key.verifying_key().to_bytes(),
            requested_at_unix: 100,
        };
        let mut signed = SignedStateFetchRequest::sign(request, &key);
        assert_eq!(signed.verify(), Ok(()));
        signed.request.requested_at_unix = 101;
        assert_eq!(signed.verify(), Err(TokenError::InvalidSignature));
        signed.signature.truncate(10);
        assert_eq!(signed.verify(), Err(TokenError::InvalidSignature));
    }

    #[test]
    fn resource_budget_validate_state_rejects_unknown_node() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        state.resource_budgets.push(ResourceBudget {
            node_pubkey: [99; 32],
            max_in_flight: 1,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::BudgetNodeUnknown)
        );
    }

    #[test]
    fn resource_budget_validate_state_rejects_duplicate() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        state.resource_budgets.push(ResourceBudget {
            node_pubkey: state.owner_pubkey,
            max_in_flight: 1,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        });
        state.resource_budgets.push(ResourceBudget {
            node_pubkey: state.owner_pubkey,
            max_in_flight: 2,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::DuplicateBudgetNode)
        );
    }

    #[test]
    fn resource_budget_validate_state_rejects_empty() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        state.resource_budgets.push(ResourceBudget {
            node_pubkey: state.owner_pubkey,
            max_in_flight: 0,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::EmptyResourceBudget)
        );
    }

    #[test]
    fn ban_overlapping_active_member_rejected() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        let member_key = SigningKey::generate(&mut rand::thread_rng());
        let member_pubkey = member_key.verifying_key().to_bytes();
        state.members.push(Member {
            node_pubkey: member_pubkey,
            endpoint: "http://example.com:7337".into(),
            relay_endpoint: None,
            joined_generation: 1,
        });
        state.banned.push(MembershipBan {
            node_pubkey: member_pubkey,
            banned_at_unix: 2000,
            reason: "test".into(),
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::BanTargetIsMember)
        );
    }

    #[test]
    fn ban_targeting_owner_rejected() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        state.banned.push(MembershipBan {
            node_pubkey: state.owner_pubkey,
            banned_at_unix: 2000,
            reason: "test".into(),
        });
        assert_eq!(
            SignedState::sign(state, &key),
            Err(StateError::BanTargetIsMember)
        );
    }

    #[test]
    fn ban_non_member_accepted() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut state = state(&key);
        let stranger = [9; 32];
        state.banned.push(MembershipBan {
            node_pubkey: stranger,
            banned_at_unix: 2000,
            reason: "spam".into(),
        });
        let signed = SignedState::sign(state, &key).unwrap();
        assert_eq!(signed.state.banned.len(), 1);
        assert_eq!(signed.state.banned[0].node_pubkey, stranger);
    }

    #[test]
    fn membership_ban_roundtrips() {
        let ban = MembershipBan {
            node_pubkey: [7; 32],
            banned_at_unix: 1234,
            reason: "abuse".into(),
        };
        let json = serde_json::to_string(&ban).unwrap();
        let parsed: MembershipBan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.node_pubkey, ban.node_pubkey);
        assert_eq!(parsed.reason, ban.reason);
    }

    #[test]
    fn abuse_report_roundtrips() {
        let report = AbuseReport {
            reporter_pubkey: [1; 32],
            subject_pubkey: [2; 32],
            category: "spam".into(),
            note: "flooding requests".into(),
            reported_at_unix: 1234,
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: AbuseReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reporter_pubkey, report.reporter_pubkey);
        assert_eq!(parsed.subject_pubkey, report.subject_pubkey);
        assert_eq!(parsed.category, report.category);
    }

    #[test]
    fn hardware_benchmark_gpu_layers_defaults_to_zero_when_absent_from_json() {
        let json = r#"{
            "model": "gemma",
            "backend": "vulkan",
            "context_size": 2048,
            "concurrency": 1,
            "prompt_tokens_per_second_milli": 10000,
            "decode_tokens_per_second_milli": 5000,
            "peak_memory_bytes": 4200000000
        }"#;
        let benchmark: HardwareBenchmark = serde_json::from_str(json).unwrap();
        assert_eq!(benchmark.gpu_layers, 0);
    }

    #[test]
    fn hardware_benchmark_gpu_layers_round_trips() {
        let benchmark = HardwareBenchmark {
            model: "gemma".into(),
            backend: "vulkan".into(),
            gpu_layers: 32,
            context_size: 2048,
            concurrency: 1,
            prompt_tokens_per_second_milli: 10_000,
            decode_tokens_per_second_milli: 5_000,
            peak_memory_bytes: 4_200_000_000,
        };
        let json = serde_json::to_value(&benchmark).unwrap();
        assert_eq!(json["gpu_layers"], 32);
        let round_tripped: HardwareBenchmark = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped, benchmark);
    }
}
