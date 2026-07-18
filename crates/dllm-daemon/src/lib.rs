use dllm_protocol::{
    now_unix, AbuseReport, AccessRequest, ForwardingPolicy, HardwareProfile, Member, MembershipBan,
    ModelAssignment, NetworkState, Placement, PlacementLifecycle, ResourceBudget,
    SignedAccessRequest, SignedJoinToken, SignedState, StateError, TokenError,
    TransportEndpointBinding, TransportEndpointRevocation, SCHEMA_VERSION,
};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

pub mod api;
pub mod audit;
pub mod backup;
pub mod budget;
pub mod credentials;
pub mod inference;
pub mod peer_service;
pub mod rate_limit;

/// Root directory for DLLM's per-user files (state, keys) when no explicit
/// path is configured via env var or CLI flag. Created eagerly so callers
/// can `fs::write` into it immediately without a separate mkdir step.
pub fn default_dir() -> std::io::Result<PathBuf> {
    default_dir_in(dirs::home_dir())
}

fn default_dir_in(home: Option<PathBuf>) -> std::io::Result<PathBuf> {
    let dir = home
        .map(|home| home.join(".dllm"))
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn default_state_path() -> std::io::Result<PathBuf> {
    Ok(default_dir()?.join("state.json"))
}

pub fn default_owner_key_path() -> std::io::Result<PathBuf> {
    Ok(default_dir()?.join("owner.key"))
}

pub fn default_node_key_path() -> std::io::Result<PathBuf> {
    Ok(default_dir()?.join("node.key"))
}

pub fn default_transport_key_path() -> std::io::Result<PathBuf> {
    Ok(default_dir()?.join("transport.key"))
}

pub fn default_config_path() -> std::io::Result<PathBuf> {
    Ok(default_dir()?.join("config.json"))
}

#[cfg(test)]
mod default_dir_tests {
    use super::default_dir_in;
    use std::path::PathBuf;

    #[test]
    fn joins_dot_dllm_onto_home_and_creates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let resolved = default_dir_in(Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(resolved, tmp.path().join(".dllm"));
        assert!(resolved.is_dir());
    }

    #[test]
    fn falls_back_to_current_dir_when_home_is_unknown() {
        let resolved = default_dir_in(None).unwrap();
        assert_eq!(resolved, PathBuf::from("."));
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("state storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("state encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("join token error: {0}")]
    Token(#[from] TokenError),
    #[error("join token belongs to another network")]
    WrongNetwork,
    #[error("join token has already been redeemed")]
    TokenUsed,
    #[error("owner key must contain exactly 32 bytes")]
    InvalidOwnerKey,
    #[error("owner key does not match persisted network owner")]
    OwnerKeyMismatch,
    #[error("this node does not hold the network owner authority")]
    OwnerAuthorityUnavailable,
    #[error("model assignment node is not a network member")]
    AssignmentNodeUnknown,
    #[error("hardware profile node is not a network member")]
    ProfileNodeUnknown,
    #[error("new owner must be a current network member")]
    TransferTargetUnknown,
    #[error("transport binding node is not a network member")]
    BindingNodeUnknown,
    #[error("transport binding generation {supplied} is stale; next generation is {next}")]
    StaleBindingGeneration { supplied: u64, next: u64 },
    #[error("transport endpoint identity is already bound to another node")]
    TransportPeerIdInUse,
    #[error("transport binding expiry must be later than its issue time")]
    InvalidBindingLifetime,
    #[error("node has no active transport binding")]
    BindingNotFound,
    #[error("transport identity is not authorized for this node")]
    TransportIdentityUnauthorized,
    #[error("transport binding expired at {0}")]
    TransportBindingExpired(u64),
    #[error("forwarding policy node is not a network member")]
    ForwardingNodeUnknown,
    #[error("node is already a network member")]
    AccessRequestAlreadyMember,
    #[error("node already has a pending access request")]
    AccessRequestAlreadyPending,
    #[error("no pending access request found for this node")]
    AccessRequestNotFound,
    #[error("access request signature does not verify against the claimed node key")]
    InvalidAccessRequestSignature,
    #[error("resource budget node is not a network member")]
    BudgetNodeUnknown,
    #[error("resource budget must allow at least one request")]
    EmptyResourceBudget,
    #[error("pending access request queue is full")]
    PendingQueueFull,
    #[error("node is banned: {0}")]
    NodeIsBanned(String),
    #[error("node is not banned")]
    BanNotFound,
    #[error("abuse report can only be submitted by a network member")]
    AbuseReportNotMember,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub schema_version: u32,
    pub signed_state: SignedState,
    pub redeemed_token_ids: HashSet<Uuid>,
    #[serde(default)]
    pub pending_access_requests: Vec<AccessRequest>,
    #[serde(default)]
    pub abuse_reports: Vec<AbuseReport>,
}

pub struct NetworkStore {
    pub owner_key: Option<SigningKey>,
    pub state: SignedState,
    used_tokens: HashSet<Uuid>,
    pending_access_requests: Vec<AccessRequest>,
    abuse_reports: Vec<AbuseReport>,
}

impl NetworkStore {
    pub fn save_owner_key(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        let owner_key = self
            .owner_key
            .as_ref()
            .ok_or(StoreError::OwnerAuthorityUnavailable)?;
        fs::write(path, owner_key.to_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn load_owner_key(path: impl AsRef<Path>) -> Result<SigningKey, StoreError> {
        let bytes = fs::read(path)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| StoreError::InvalidOwnerKey)?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    pub fn create(name: impl Into<String>) -> Self {
        let owner_key = SigningKey::generate(&mut rand::thread_rng());
        let state = NetworkState {
            schema_version: SCHEMA_VERSION,
            network_id: Uuid::new_v4(),
            name: name.into(),
            owner_pubkey: owner_key.verifying_key().to_bytes(),
            generation: 1,
            members: Vec::new(),
            model_assignments: Vec::new(),
            placements: Vec::new(),
            hardware_profiles: Vec::new(),
            transport_bindings: Vec::new(),
            transport_revocations: Vec::new(),
            forwarding_policy: Vec::new(),
            resource_budgets: Vec::new(),
            banned: Vec::new(),
        };
        let signed = SignedState::sign(state, &owner_key).expect("new owner state is valid");
        Self {
            owner_key: Some(owner_key),
            state: signed,
            used_tokens: HashSet::new(),
            pending_access_requests: Vec::new(),
            abuse_reports: Vec::new(),
        }
    }

    pub fn load(
        state_path: impl AsRef<Path>,
        owner_key_path: impl AsRef<Path>,
    ) -> Result<Self, StoreError> {
        let persisted: PersistedState = serde_json::from_slice(&fs::read(state_path)?)?;
        if persisted.schema_version != SCHEMA_VERSION {
            return Err(StoreError::State(StateError::SchemaVersion(
                persisted.schema_version,
            )));
        }
        persisted.signed_state.verify()?;
        let owner_key = Self::load_owner_key(owner_key_path)?;
        if owner_key.verifying_key().to_bytes() != persisted.signed_state.state.owner_pubkey {
            return Err(StoreError::OwnerKeyMismatch);
        }
        Ok(Self {
            owner_key: Some(owner_key),
            state: persisted.signed_state,
            used_tokens: persisted.redeemed_token_ids,
            pending_access_requests: persisted.pending_access_requests,
            abuse_reports: persisted.abuse_reports,
        })
    }

    pub fn load_replica(state_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let persisted: PersistedState = serde_json::from_slice(&fs::read(state_path)?)?;
        if persisted.schema_version != SCHEMA_VERSION {
            return Err(StoreError::State(StateError::SchemaVersion(
                persisted.schema_version,
            )));
        }
        persisted.signed_state.verify()?;
        Ok(Self {
            owner_key: None,
            state: persisted.signed_state,
            used_tokens: persisted.redeemed_token_ids,
            pending_access_requests: persisted.pending_access_requests,
            abuse_reports: persisted.abuse_reports,
        })
    }

    #[cfg(test)]
    fn issue_join_token(
        &self,
        owner_endpoint: String,
        expires_at_unix: Option<u64>,
    ) -> SignedJoinToken {
        self.try_issue_join_token(owner_endpoint, expires_at_unix)
            .expect("only an owner store can issue join tokens")
    }

    pub fn try_issue_join_token(
        &self,
        owner_endpoint: String,
        expires_at_unix: Option<u64>,
    ) -> Result<SignedJoinToken, StoreError> {
        let owner_key = self
            .owner_key
            .as_ref()
            .ok_or(StoreError::OwnerAuthorityUnavailable)?;
        Ok(SignedJoinToken::issue(
            self.state.state.network_id,
            owner_key,
            owner_endpoint,
            expires_at_unix,
        ))
    }

    pub fn redeem_join_token(
        &mut self,
        token: SignedJoinToken,
        node_pubkey: [u8; 32],
        node_endpoint: String,
    ) -> Result<(), StoreError> {
        token.verify(now_unix())?;
        if token.token.network_id != self.state.state.network_id
            || token.token.owner_pubkey != self.state.state.owner_pubkey
        {
            return Err(StoreError::WrongNetwork);
        }
        if !self.used_tokens.insert(token.token.token_id) {
            return Err(StoreError::TokenUsed);
        }
        let mut next = self.state.state.clone();
        next.generation += 1;
        next.members.push(Member {
            node_pubkey,
            endpoint: node_endpoint,
            relay_endpoint: None,
            joined_generation: next.generation,
        });
        self.state = self.sign(next)?;
        Ok(())
    }

    const MAX_PENDING_ACCESS_REQUESTS: usize = 1000;

    pub fn submit_access_request(&mut self, signed: SignedAccessRequest) -> Result<(), StoreError> {
        signed
            .verify()
            .map_err(|_| StoreError::InvalidAccessRequestSignature)?;
        let node_pubkey = signed.request.node_pubkey;
        // Reject banned identities before queuing.
        if self
            .state
            .state
            .banned
            .iter()
            .any(|ban| ban.node_pubkey == node_pubkey)
        {
            return Err(StoreError::NodeIsBanned(
                self.state
                    .state
                    .banned
                    .iter()
                    .find(|ban| ban.node_pubkey == node_pubkey)
                    .map(|ban| ban.reason.clone())
                    .unwrap_or_default(),
            ));
        }
        if self.state.state.owner_pubkey == node_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == node_pubkey)
        {
            return Err(StoreError::AccessRequestAlreadyMember);
        }
        if self
            .pending_access_requests
            .iter()
            .any(|req| req.node_pubkey == node_pubkey)
        {
            return Err(StoreError::AccessRequestAlreadyPending);
        }
        if self.pending_access_requests.len() >= Self::MAX_PENDING_ACCESS_REQUESTS {
            return Err(StoreError::PendingQueueFull);
        }
        self.pending_access_requests.push(signed.request);
        Ok(())
    }

    pub fn list_access_requests(&self) -> &[AccessRequest] {
        &self.pending_access_requests
    }

    pub fn approve_access_request(
        &mut self,
        node_pubkey: [u8; 32],
        endpoint: String,
    ) -> Result<(), StoreError> {
        let idx = self
            .pending_access_requests
            .iter()
            .position(|req| req.node_pubkey == node_pubkey)
            .ok_or(StoreError::AccessRequestNotFound)?;
        self.pending_access_requests.remove(idx);
        let mut next = self.state.state.clone();
        next.generation += 1;
        next.members.push(Member {
            node_pubkey,
            endpoint,
            relay_endpoint: None,
            joined_generation: next.generation,
        });
        self.state = self.sign(next)?;
        Ok(())
    }

    pub fn deny_access_request(&mut self, node_pubkey: [u8; 32]) -> Result<(), StoreError> {
        let idx = self
            .pending_access_requests
            .iter()
            .position(|req| req.node_pubkey == node_pubkey)
            .ok_or(StoreError::AccessRequestNotFound)?;
        self.pending_access_requests.remove(idx);
        Ok(())
    }

    pub fn set_resource_budget(
        &mut self,
        node_pubkey: [u8; 32],
        max_in_flight: u32,
        max_requests_per_window: u32,
        window_seconds: u32,
    ) -> Result<bool, StoreError> {
        let known = node_pubkey == self.state.state.owner_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == node_pubkey);
        if !known {
            return Err(StoreError::BudgetNodeUnknown);
        }
        if max_in_flight == 0 && max_requests_per_window == 0 {
            return Err(StoreError::EmptyResourceBudget);
        }
        let mut next = self.state.state.clone();
        let previous = next
            .resource_budgets
            .iter()
            .find(|budget| budget.node_pubkey == node_pubkey)
            .cloned();
        next.resource_budgets
            .retain(|budget| budget.node_pubkey != node_pubkey);
        // Preserve the previous granted_generation if the values are unchanged,
        // so the idempotent guard works correctly.
        let granted_generation = previous
            .as_ref()
            .filter(|prev| {
                prev.max_in_flight == max_in_flight
                    && prev.max_requests_per_window == max_requests_per_window
                    && prev.window_seconds == window_seconds
            })
            .map(|prev| prev.granted_generation)
            .unwrap_or(next.generation + 1);
        next.resource_budgets.push(ResourceBudget {
            node_pubkey,
            max_in_flight,
            max_requests_per_window,
            window_seconds,
            granted_generation,
        });
        next.resource_budgets
            .sort_by_key(|budget| budget.node_pubkey);
        let current = next
            .resource_budgets
            .iter()
            .find(|budget| budget.node_pubkey == node_pubkey);
        if previous.as_ref() == current {
            return Ok(false);
        }
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn remove_resource_budget(&mut self, node_pubkey: [u8; 32]) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let before = next.resource_budgets.len();
        next.resource_budgets
            .retain(|budget| budget.node_pubkey != node_pubkey);
        if next.resource_budgets.len() == before {
            return Ok(false);
        }
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn ban_node(&mut self, node_pubkey: [u8; 32], reason: String) -> Result<bool, StoreError> {
        // validate_state rejects a ban overlapping an active member, so the caller
        // must revoke first.
        if self
            .state
            .state
            .banned
            .iter()
            .any(|ban| ban.node_pubkey == node_pubkey)
        {
            return Ok(false);
        }
        let mut next = self.state.state.clone();
        next.banned.push(MembershipBan {
            node_pubkey,
            banned_at_unix: now_unix(),
            reason,
        });
        next.banned.sort_by_key(|ban| ban.node_pubkey);
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn unban_node(&mut self, node_pubkey: [u8; 32]) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let before = next.banned.len();
        next.banned.retain(|ban| ban.node_pubkey != node_pubkey);
        if next.banned.len() == before {
            return Err(StoreError::BanNotFound);
        }
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn submit_abuse_report(&mut self, report: AbuseReport) -> Result<(), StoreError> {
        let is_member = self.state.state.owner_pubkey == report.reporter_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == report.reporter_pubkey);
        if !is_member {
            return Err(StoreError::AbuseReportNotMember);
        }
        self.abuse_reports.push(report);
        Ok(())
    }

    pub fn list_abuse_reports(&self) -> &[AbuseReport] {
        &self.abuse_reports
    }

    pub fn clear_abuse_report(&mut self, index: usize) -> Result<(), StoreError> {
        if index >= self.abuse_reports.len() {
            return Err(StoreError::AccessRequestNotFound);
        }
        self.abuse_reports.remove(index);
        Ok(())
    }

    pub fn revoke_member(&mut self, node_pubkey: [u8; 32]) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let before = next.members.len();
        next.members
            .retain(|member| member.node_pubkey != node_pubkey);
        if next.members.len() == before {
            return Ok(false);
        }
        next.generation += 1;
        next.model_assignments
            .retain(|assignment| assignment.node_pubkey != node_pubkey);
        next.placements
            .retain(|placement| placement.node_pubkey != node_pubkey);
        next.hardware_profiles
            .retain(|profile| profile.node_pubkey != node_pubkey);
        if let Some(binding) = next
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == node_pubkey)
            .cloned()
        {
            next.transport_revocations
                .push(TransportEndpointRevocation {
                    node_pubkey,
                    transport_peer_id: binding.transport_peer_id,
                    binding_generation: binding.binding_generation,
                    revoked_at_unix: now_unix(),
                });
        }
        next.transport_bindings
            .retain(|binding| binding.node_pubkey != node_pubkey);
        next.forwarding_policy
            .retain(|policy| policy.node_pubkey != node_pubkey);
        next.resource_budgets
            .retain(|budget| budget.node_pubkey != node_pubkey);
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn bind_transport_endpoint(
        &mut self,
        node_pubkey: [u8; 32],
        transport_peer_id: String,
        binding_generation: u64,
        issued_at_unix: u64,
        expires_at_unix: u64,
    ) -> Result<(), StoreError> {
        let known = node_pubkey == self.state.state.owner_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == node_pubkey);
        if !known {
            return Err(StoreError::BindingNodeUnknown);
        }
        if expires_at_unix <= issued_at_unix {
            return Err(StoreError::InvalidBindingLifetime);
        }
        let next_generation = self.next_binding_generation(node_pubkey);
        if binding_generation != next_generation {
            return Err(StoreError::StaleBindingGeneration {
                supplied: binding_generation,
                next: next_generation,
            });
        }
        if self.state.state.transport_bindings.iter().any(|binding| {
            binding.transport_peer_id == transport_peer_id && binding.node_pubkey != node_pubkey
        }) || self
            .state
            .state
            .transport_revocations
            .iter()
            .any(|revocation| revocation.transport_peer_id == transport_peer_id)
        {
            return Err(StoreError::TransportPeerIdInUse);
        }
        let mut next = self.state.state.clone();
        if let Some(previous) = next
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == node_pubkey)
            .cloned()
        {
            next.transport_revocations
                .push(TransportEndpointRevocation {
                    node_pubkey,
                    transport_peer_id: previous.transport_peer_id,
                    binding_generation: previous.binding_generation,
                    revoked_at_unix: issued_at_unix,
                });
        }
        next.transport_bindings
            .retain(|binding| binding.node_pubkey != node_pubkey);
        next.transport_bindings.push(TransportEndpointBinding {
            node_pubkey,
            transport_peer_id,
            binding_generation,
            issued_at_unix,
            expires_at_unix,
        });
        next.transport_bindings
            .sort_by_key(|binding| binding.node_pubkey);
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(())
    }

    pub fn revoke_transport_endpoint(
        &mut self,
        node_pubkey: [u8; 32],
        revoked_at_unix: u64,
    ) -> Result<TransportEndpointRevocation, StoreError> {
        let mut next = self.state.state.clone();
        let binding = next
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == node_pubkey)
            .cloned()
            .ok_or(StoreError::BindingNotFound)?;
        let revocation = TransportEndpointRevocation {
            node_pubkey,
            transport_peer_id: binding.transport_peer_id,
            binding_generation: binding.binding_generation,
            revoked_at_unix,
        };
        next.transport_bindings
            .retain(|candidate| candidate.node_pubkey != node_pubkey);
        next.transport_revocations.push(revocation.clone());
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(revocation)
    }

    pub fn authorize_transport_endpoint(
        &self,
        node_pubkey: [u8; 32],
        transport_peer_id: &str,
        now_unix: u64,
    ) -> Result<&TransportEndpointBinding, StoreError> {
        let binding = self
            .state
            .state
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == node_pubkey)
            .ok_or(StoreError::TransportIdentityUnauthorized)?;
        if binding.transport_peer_id != transport_peer_id {
            return Err(StoreError::TransportIdentityUnauthorized);
        }
        if now_unix >= binding.expires_at_unix {
            return Err(StoreError::TransportBindingExpired(binding.expires_at_unix));
        }
        Ok(binding)
    }

    pub fn next_binding_generation(&self, node_pubkey: [u8; 32]) -> u64 {
        self.state
            .state
            .transport_bindings
            .iter()
            .filter(|binding| binding.node_pubkey == node_pubkey)
            .map(|binding| binding.binding_generation)
            .chain(
                self.state
                    .state
                    .transport_revocations
                    .iter()
                    .filter(|revocation| revocation.node_pubkey == node_pubkey)
                    .map(|revocation| revocation.binding_generation),
            )
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    pub fn set_forwarding_policy(
        &mut self,
        node_pubkey: [u8; 32],
        max_reservations: Option<u32>,
    ) -> Result<bool, StoreError> {
        let known = node_pubkey == self.state.state.owner_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == node_pubkey);
        if !known {
            return Err(StoreError::ForwardingNodeUnknown);
        }
        let mut next = self.state.state.clone();
        let previous = next
            .forwarding_policy
            .iter()
            .find(|policy| policy.node_pubkey == node_pubkey)
            .cloned();
        next.forwarding_policy
            .retain(|policy| policy.node_pubkey != node_pubkey);
        if let Some(max_reservations) = max_reservations {
            next.forwarding_policy.push(ForwardingPolicy {
                node_pubkey,
                max_reservations,
            });
            next.forwarding_policy
                .sort_by_key(|policy| policy.node_pubkey);
        }
        let current = next
            .forwarding_policy
            .iter()
            .find(|policy| policy.node_pubkey == node_pubkey);
        if previous.as_ref() == current {
            return Ok(false);
        }
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn assign_model(
        &mut self,
        model: String,
        node_pubkey: [u8; 32],
    ) -> Result<bool, StoreError> {
        let owner = node_pubkey == self.state.state.owner_pubkey;
        let member = self
            .state
            .state
            .members
            .iter()
            .any(|candidate| candidate.node_pubkey == node_pubkey);
        if !owner && !member {
            return Err(StoreError::AssignmentNodeUnknown);
        }
        if self
            .state
            .state
            .model_assignments
            .iter()
            .any(|assignment| assignment.model == model && assignment.node_pubkey == node_pubkey)
        {
            return Ok(false);
        }
        let mut next = self.state.state.clone();
        next.generation += 1;
        next.model_assignments.push(ModelAssignment {
            model: model.clone(),
            node_pubkey,
        });
        next.placements.push(Placement {
            placement_id: Uuid::new_v4(),
            model,
            node_pubkey,
            created_generation: next.generation,
            lifecycle: PlacementLifecycle::Ready,
        });
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn unassign_model(
        &mut self,
        model: &str,
        node_pubkey: [u8; 32],
    ) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let before = next.model_assignments.len();
        next.model_assignments.retain(|assignment| {
            assignment.model != model || assignment.node_pubkey != node_pubkey
        });
        if next.model_assignments.len() == before {
            return Ok(false);
        }
        next.placements
            .retain(|placement| placement.model != model || placement.node_pubkey != node_pubkey);
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn set_placement_draining(
        &mut self,
        placement_id: Uuid,
        draining: bool,
    ) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let Some(placement) = next
            .placements
            .iter_mut()
            .find(|placement| placement.placement_id == placement_id)
        else {
            return Ok(false);
        };
        let lifecycle = if draining {
            PlacementLifecycle::Draining
        } else {
            PlacementLifecycle::Ready
        };
        if placement.lifecycle == lifecycle {
            return Ok(false);
        }
        placement.lifecycle = lifecycle;
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn transfer_owner(
        &mut self,
        new_owner_key: SigningKey,
        old_owner_endpoint: String,
    ) -> Result<(), StoreError> {
        let old_owner = self.state.state.owner_pubkey;
        let new_owner = new_owner_key.verifying_key().to_bytes();
        if !self
            .state
            .state
            .members
            .iter()
            .any(|member| member.node_pubkey == new_owner)
        {
            return Err(StoreError::TransferTargetUnknown);
        }
        let mut next = self.state.state.clone();
        next.generation += 1;
        next.owner_pubkey = new_owner;
        next.members
            .retain(|member| member.node_pubkey != new_owner);
        next.members.push(Member {
            node_pubkey: old_owner,
            endpoint: old_owner_endpoint,
            relay_endpoint: None,
            joined_generation: next.generation,
        });
        self.state = SignedState::sign(next, &new_owner_key)?;
        self.owner_key = Some(new_owner_key);
        Ok(())
    }

    pub fn publish_hardware_profile(
        &mut self,
        profile: HardwareProfile,
    ) -> Result<bool, StoreError> {
        let known = profile.node_pubkey == self.state.state.owner_pubkey
            || self
                .state
                .state
                .members
                .iter()
                .any(|member| member.node_pubkey == profile.node_pubkey);
        if !known {
            return Err(StoreError::ProfileNodeUnknown);
        }
        if self
            .state
            .state
            .hardware_profiles
            .iter()
            .any(|candidate| candidate == &profile)
        {
            return Ok(false);
        }
        let mut next = self.state.state.clone();
        next.hardware_profiles
            .retain(|candidate| candidate.node_pubkey != profile.node_pubkey);
        next.hardware_profiles.push(profile);
        next.hardware_profiles
            .sort_by_key(|candidate| candidate.node_pubkey);
        next.generation += 1;
        self.state = self.sign(next)?;
        Ok(true)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        let persisted = PersistedState {
            schema_version: SCHEMA_VERSION,
            signed_state: self.state.clone(),
            redeemed_token_ids: self.used_tokens.clone(),
            pending_access_requests: self.pending_access_requests.clone(),
            abuse_reports: self.abuse_reports.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn sign(&self, state: NetworkState) -> Result<SignedState, StoreError> {
        let owner_key = self
            .owner_key
            .as_ref()
            .ok_or(StoreError::OwnerAuthorityUnavailable)?;
        Ok(SignedState::sign(state, owner_key)?)
    }

    pub fn random_node_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER_A: &str = "12D3KooWSahP5pFRCEfaziPEba7urXGeif6T1y8jmodzdFUvzBHj";
    const PEER_B: &str = "12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY";

    #[test]
    fn redemption_advances_signed_generation_and_is_single_use() {
        let mut store = NetworkStore::create("test");
        let token = store.issue_join_token("http://127.0.0.1:7337".into(), None);
        let node = NetworkStore::random_node_key();
        store
            .redeem_join_token(token.clone(), node, "http://node:7337".into())
            .unwrap();
        assert_eq!(store.state.state.generation, 2);
        assert_eq!(store.state.state.members.len(), 1);
        assert!(store.state.verify().is_ok());
        assert!(matches!(
            store.redeem_join_token(
                token,
                NetworkStore::random_node_key(),
                "http://node:7337".into()
            ),
            Err(StoreError::TokenUsed)
        ));
    }

    #[test]
    fn transport_bindings_rotate_revoke_expire_and_reject_replay() {
        let mut store = NetworkStore::create("test");
        let node = store.state.state.owner_pubkey;
        store
            .bind_transport_endpoint(node, PEER_A.into(), 1, 100, 200)
            .unwrap();
        assert!(store
            .authorize_transport_endpoint(node, PEER_A, 199)
            .is_ok());
        assert!(matches!(
            store.authorize_transport_endpoint(node, PEER_A, 200),
            Err(StoreError::TransportBindingExpired(200))
        ));

        store
            .bind_transport_endpoint(node, PEER_B.into(), 2, 150, 300)
            .unwrap();
        assert!(matches!(
            store.authorize_transport_endpoint(node, PEER_A, 175),
            Err(StoreError::TransportIdentityUnauthorized)
        ));
        assert!(matches!(
            store.bind_transport_endpoint(node, PEER_A.into(), 1, 175, 400),
            Err(StoreError::StaleBindingGeneration {
                supplied: 1,
                next: 3
            })
        ));
        assert!(matches!(
            store.bind_transport_endpoint(node, PEER_A.into(), 3, 175, 400),
            Err(StoreError::TransportPeerIdInUse)
        ));

        let revocation = store.revoke_transport_endpoint(node, 180).unwrap();
        assert_eq!(revocation.transport_peer_id, PEER_B);
        assert_eq!(store.next_binding_generation(node), 3);
        assert!(matches!(
            store.authorize_transport_endpoint(node, PEER_B, 181),
            Err(StoreError::TransportIdentityUnauthorized)
        ));
        assert!(store.state.verify().is_ok());
    }

    #[test]
    fn transport_binding_tombstones_survive_persistence() {
        let directory = std::env::temp_dir().join(format!("dllm-binding-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let state_path = directory.join("state.json");
        let key_path = directory.join("owner.key");
        let mut store = NetworkStore::create("test");
        let node = store.state.state.owner_pubkey;
        store
            .bind_transport_endpoint(node, PEER_A.into(), 1, 100, 200)
            .unwrap();
        store.revoke_transport_endpoint(node, 150).unwrap();
        store.save_owner_key(&key_path).unwrap();
        store.save(&state_path).unwrap();

        let mut loaded = NetworkStore::load(&state_path, &key_path).unwrap();
        assert_eq!(loaded.next_binding_generation(node), 2);
        assert!(matches!(
            loaded.bind_transport_endpoint(node, PEER_A.into(), 1, 160, 300),
            Err(StoreError::StaleBindingGeneration {
                supplied: 1,
                next: 2
            })
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn member_revocation_tombstones_its_transport_binding() {
        let mut store = NetworkStore::create("test");
        let node = NetworkStore::random_node_key();
        let token = store.issue_join_token("http://owner".into(), None);
        store
            .redeem_join_token(token, node, "http://member".into())
            .unwrap();
        store
            .bind_transport_endpoint(node, PEER_A.into(), 1, 100, 200)
            .unwrap();

        assert!(store.revoke_member(node).unwrap());
        assert!(store.state.state.transport_bindings.is_empty());
        assert!(store
            .state
            .state
            .transport_revocations
            .iter()
            .any(|revocation| {
                revocation.node_pubkey == node && revocation.transport_peer_id == PEER_A
            }));
        assert!(store.state.verify().is_ok());
    }

    #[test]
    fn forwarding_eligibility_is_owner_signed_and_removed_with_membership() {
        let mut store = NetworkStore::create("test");
        let node = NetworkStore::random_node_key();
        let token = store.issue_join_token("http://owner".into(), None);
        store
            .redeem_join_token(token, node, "http://member".into())
            .unwrap();
        let generation = store.state.state.generation;

        assert!(store.set_forwarding_policy(node, Some(4)).unwrap());
        assert_eq!(store.state.state.generation, generation + 1);
        assert_eq!(store.state.state.forwarding_policy[0].max_reservations, 4);
        assert!(store.state.verify().is_ok());
        assert!(!store.set_forwarding_policy(node, Some(4)).unwrap());
        assert!(store.revoke_member(node).unwrap());
        assert!(store.state.state.forwarding_policy.is_empty());
        assert!(store.state.verify().is_ok());
    }

    #[test]
    fn signed_state_replica_verifies_without_owner_authority() {
        let directory = std::env::temp_dir().join(format!("dllm-replica-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let state_path = directory.join("state.json");
        let owner = NetworkStore::create("test");
        owner.save(&state_path).unwrap();

        let mut replica = NetworkStore::load_replica(&state_path).unwrap();
        assert!(replica.owner_key.is_none());
        assert!(replica.state.verify().is_ok());
        assert!(matches!(
            replica.set_forwarding_policy(replica.state.state.owner_pubkey, Some(1)),
            Err(StoreError::OwnerAuthorityUnavailable)
        ));
        fs::remove_file(state_path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn revocation_advances_generation_and_is_idempotent() {
        let mut store = NetworkStore::create("test");
        let node = NetworkStore::random_node_key();
        store
            .redeem_join_token(
                store.issue_join_token("http://127.0.0.1:7337".into(), None),
                node,
                "http://node:7337".into(),
            )
            .unwrap();
        assert!(store.revoke_member(node).unwrap());
        assert_eq!(store.state.state.generation, 3);
        assert!(store.state.state.members.is_empty());
        assert!(!store.revoke_member(node).unwrap());
        assert!(store.state.verify().is_ok());
    }

    #[test]
    fn owner_key_round_trips() {
        let store = NetworkStore::create("test");
        let path = std::env::temp_dir().join(format!("dllmd-key-{}", Uuid::new_v4()));
        store.save_owner_key(&path).unwrap();
        let loaded = NetworkStore::load_owner_key(&path).unwrap();
        assert_eq!(
            loaded.verifying_key(),
            store.owner_key.as_ref().unwrap().verifying_key()
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn persisted_redemption_survives_restart() {
        let mut store = NetworkStore::create("test");
        let token = store.issue_join_token("http://127.0.0.1:7337".into(), None);
        store
            .redeem_join_token(
                token.clone(),
                NetworkStore::random_node_key(),
                "http://node:7337".into(),
            )
            .unwrap();
        let suffix = Uuid::new_v4();
        let state_path = std::env::temp_dir().join(format!("dllmd-state-{suffix}.json"));
        let key_path = std::env::temp_dir().join(format!("dllmd-key-{suffix}"));
        store.save(&state_path).unwrap();
        store.save_owner_key(&key_path).unwrap();
        let mut loaded = NetworkStore::load(&state_path, &key_path).unwrap();
        assert!(matches!(
            loaded.redeem_join_token(
                token,
                NetworkStore::random_node_key(),
                "http://node:7337".into()
            ),
            Err(StoreError::TokenUsed)
        ));
        std::fs::remove_file(state_path).unwrap();
        std::fs::remove_file(key_path).unwrap();
    }

    #[test]
    fn model_assignment_creates_and_removes_placement() {
        let mut store = NetworkStore::create("test");
        let owner = store.state.state.owner_pubkey;
        assert!(store.assign_model("qwen".into(), owner).unwrap());
        assert_eq!(store.state.state.generation, 2);
        assert_eq!(store.state.state.model_assignments.len(), 1);
        assert_eq!(store.state.state.placements.len(), 1);
        assert!(!store.assign_model("qwen".into(), owner).unwrap());
        assert!(store.unassign_model("qwen", owner).unwrap());
        assert_eq!(store.state.state.generation, 3);
        assert!(store.state.state.placements.is_empty());
        assert!(store.state.verify().is_ok());
    }

    #[test]
    fn placement_drain_is_signed_and_idempotent() {
        let mut store = NetworkStore::create("test");
        let owner = store.state.state.owner_pubkey;
        store.assign_model("qwen".into(), owner).unwrap();
        let placement_id = store.state.state.placements[0].placement_id;
        assert!(store.set_placement_draining(placement_id, true).unwrap());
        assert_eq!(store.state.state.generation, 3);
        assert_eq!(
            store.state.state.placements[0].lifecycle,
            PlacementLifecycle::Draining
        );
        assert!(!store.set_placement_draining(placement_id, true).unwrap());
        assert!(store.set_placement_draining(placement_id, false).unwrap());
        assert_eq!(
            store.state.state.placements[0].lifecycle,
            PlacementLifecycle::Ready
        );
        store.state.verify().unwrap();
    }

    #[test]
    fn owner_transfer_moves_authority_without_unsigned_state() {
        let mut store = NetworkStore::create("test");
        let old_owner = store.state.state.owner_pubkey;
        let new_owner_key = SigningKey::generate(&mut rand::thread_rng());
        let new_owner = new_owner_key.verifying_key().to_bytes();
        let token = store.issue_join_token("http://old-owner".into(), None);
        store
            .redeem_join_token(token, new_owner, "http://new-owner".into())
            .unwrap();
        store
            .transfer_owner(new_owner_key, "http://old-owner".into())
            .unwrap();
        assert_eq!(store.state.state.owner_pubkey, new_owner);
        assert!(store
            .state
            .state
            .members
            .iter()
            .any(|member| member.node_pubkey == old_owner));
        assert!(!store
            .state
            .state
            .members
            .iter()
            .any(|member| member.node_pubkey == new_owner));
        store.state.verify().unwrap();
        assert_eq!(
            store.owner_key.as_ref().unwrap().verifying_key().to_bytes(),
            new_owner
        );
    }

    // ── P4.5 access request and resource budget tests ──

    use dllm_protocol::{AccessRequest, SignedAccessRequest};
    use ed25519_dalek::SigningKey;

    fn node_signing_key() -> SigningKey {
        SigningKey::generate(&mut rand::thread_rng())
    }

    #[test]
    fn submit_access_request_stores_pending() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        let request = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: "hi".into(),
            requested_at_unix: 100,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        store.submit_access_request(signed).unwrap();
        assert_eq!(store.list_access_requests().len(), 1);
        assert_eq!(store.list_access_requests()[0].node_pubkey, node_pubkey);
    }

    #[test]
    fn submit_access_request_rejects_duplicate() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        let request = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 100,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        store.submit_access_request(signed).unwrap();
        let request2 = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 101,
        };
        let signed2 = SignedAccessRequest::sign(request2, &key);
        assert!(matches!(
            store.submit_access_request(signed2),
            Err(StoreError::AccessRequestAlreadyPending)
        ));
    }

    #[test]
    fn submit_access_request_rejects_existing_member() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        // First, join the node as a member via token redemption.
        let token = store
            .try_issue_join_token("http://owner:7337".into(), None)
            .unwrap();
        store
            .redeem_join_token(token, node_pubkey, "http://10.0.0.1:7337".into())
            .unwrap();
        // Now try to submit an access request as the same node.
        let request = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 100,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        assert!(matches!(
            store.submit_access_request(signed),
            Err(StoreError::AccessRequestAlreadyMember)
        ));
    }

    #[test]
    fn approve_then_deny_access_request() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        // Submit.
        let request = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 100,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        store.submit_access_request(signed).unwrap();
        // Approve.
        store
            .approve_access_request(node_pubkey, "http://10.0.0.1:7337".into())
            .unwrap();
        assert!(store.list_access_requests().is_empty());
        assert!(store
            .state
            .state
            .members
            .iter()
            .any(|m| m.node_pubkey == node_pubkey));
    }

    #[test]
    fn deny_access_request_removes_pending() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        let request = AccessRequest {
            node_pubkey,
            requested_endpoint: "http://10.0.0.1:7337".into(),
            note: String::new(),
            requested_at_unix: 100,
        };
        let signed = SignedAccessRequest::sign(request, &key);
        store.submit_access_request(signed).unwrap();
        let gen_before = store.state.state.generation;
        store.deny_access_request(node_pubkey).unwrap();
        assert!(store.list_access_requests().is_empty());
        // Denial does not bump generation.
        assert_eq!(store.state.state.generation, gen_before);
        // Node is not a member.
        assert!(!store
            .state
            .state
            .members
            .iter()
            .any(|m| m.node_pubkey == node_pubkey));
    }

    #[test]
    fn set_and_remove_resource_budget() {
        let mut store = NetworkStore::create("test");
        let node_pubkey = store.state.state.owner_pubkey;
        // Set budget.
        let changed = store.set_resource_budget(node_pubkey, 5, 100, 60).unwrap();
        assert!(changed);
        assert_eq!(store.state.state.resource_budgets.len(), 1);
        assert_eq!(store.state.state.resource_budgets[0].max_in_flight, 5);
        // Idempotent set returns false.
        let changed = store.set_resource_budget(node_pubkey, 5, 100, 60).unwrap();
        assert!(!changed);
        // Remove budget.
        let changed = store.remove_resource_budget(node_pubkey).unwrap();
        assert!(changed);
        assert!(store.state.state.resource_budgets.is_empty());
        // Idempotent remove returns false.
        let changed = store.remove_resource_budget(node_pubkey).unwrap();
        assert!(!changed);
    }

    #[test]
    fn resource_budget_rejects_unknown_node() {
        let mut store = NetworkStore::create("test");
        assert!(matches!(
            store.set_resource_budget([99; 32], 1, 0, 0),
            Err(StoreError::BudgetNodeUnknown)
        ));
    }

    #[test]
    fn resource_budget_rejects_empty() {
        let mut store = NetworkStore::create("test");
        let node_pubkey = store.state.state.owner_pubkey;
        assert!(matches!(
            store.set_resource_budget(node_pubkey, 0, 0, 0),
            Err(StoreError::EmptyResourceBudget)
        ));
    }

    #[test]
    fn revoke_member_strips_resource_budget() {
        let mut store = NetworkStore::create("test");
        let key = node_signing_key();
        let node_pubkey = key.verifying_key().to_bytes();
        // Add as member.
        let token = store
            .try_issue_join_token("http://owner:7337".into(), None)
            .unwrap();
        store
            .redeem_join_token(token, node_pubkey, "http://10.0.0.1:7337".into())
            .unwrap();
        // Set budget.
        store.set_resource_budget(node_pubkey, 3, 50, 30).unwrap();
        assert_eq!(store.state.state.resource_budgets.len(), 1);
        // Revoke member.
        store.revoke_member(node_pubkey).unwrap();
        // Budget is cleaned up.
        assert!(store.state.state.resource_budgets.is_empty());
        assert!(!store
            .state
            .state
            .members
            .iter()
            .any(|m| m.node_pubkey == node_pubkey));
    }

    #[test]
    fn resource_budget_independent_of_forwarding_policy() {
        let mut store = NetworkStore::create("test");
        let node_pubkey = store.state.state.owner_pubkey;
        // Set budget without forwarding.
        store.set_resource_budget(node_pubkey, 1, 10, 60).unwrap();
        assert!(store.state.state.forwarding_policy.is_empty());
        // Set forwarding without affecting budget.
        store.set_forwarding_policy(node_pubkey, Some(5)).unwrap();
        assert_eq!(store.state.state.resource_budgets.len(), 1);
        assert_eq!(store.state.state.forwarding_policy.len(), 1);
        // Remove budget leaves forwarding intact.
        store.remove_resource_budget(node_pubkey).unwrap();
        assert!(store.state.state.resource_budgets.is_empty());
        assert_eq!(store.state.state.forwarding_policy.len(), 1);
    }
}
