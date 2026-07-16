use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkState {
    pub schema_version: u32,
    pub network_id: Uuid,
    pub name: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardingPolicy {
    pub node_pubkey: [u8; 32],
    pub max_reservations: u32,
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("state owner key does not match signer")]
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
}

impl SignedState {
    pub fn sign(state: NetworkState, key: &SigningKey) -> Result<Self, StateError> {
        validate_state(&state, key.verifying_key().as_bytes())?;
        let bytes = serde_json::to_vec(&state).expect("protocol state is serializable");
        let signature = key.sign(&bytes).to_bytes().to_vec();
        Ok(Self { state, signature })
    }

    pub fn verify(&self) -> Result<(), StateError> {
        validate_state(&self.state, &self.state.owner_pubkey)?;
        let key = VerifyingKey::from_bytes(&self.state.owner_pubkey)
            .map_err(|_| StateError::InvalidSignature)?;
        let bytes = serde_json::to_vec(&self.state).expect("protocol state is serializable");
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
    if state.schema_version != SCHEMA_VERSION {
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
    Ok(())
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
}
