use dllm_protocol::{
    HardwareProfile, Member, ModelAssignment, NetworkState, Placement, SignedJoinToken,
    SignedState, StateError, TokenError, SCHEMA_VERSION,
};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

pub mod api;

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
    #[error("model assignment node is not a network member")]
    AssignmentNodeUnknown,
    #[error("hardware profile node is not a network member")]
    ProfileNodeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub schema_version: u32,
    pub signed_state: SignedState,
    pub redeemed_token_ids: HashSet<Uuid>,
}

pub struct NetworkStore {
    pub owner_key: SigningKey,
    pub state: SignedState,
    used_tokens: HashSet<Uuid>,
}

impl NetworkStore {
    pub fn save_owner_key(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        fs::write(path, self.owner_key.to_bytes())?;
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
        };
        let signed = SignedState::sign(state, &owner_key).expect("new owner state is valid");
        Self {
            owner_key,
            state: signed,
            used_tokens: HashSet::new(),
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
            owner_key,
            state: persisted.signed_state,
            used_tokens: persisted.redeemed_token_ids,
        })
    }

    pub fn issue_join_token(
        &self,
        owner_endpoint: String,
        expires_at_unix: Option<u64>,
    ) -> SignedJoinToken {
        SignedJoinToken::issue(
            self.state.state.network_id,
            &self.owner_key,
            owner_endpoint,
            expires_at_unix,
        )
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
            joined_generation: next.generation,
        });
        self.state = SignedState::sign(next, &self.owner_key)?;
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
        self.state = SignedState::sign(next, &self.owner_key)?;
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
        });
        self.state = SignedState::sign(next, &self.owner_key)?;
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
        self.state = SignedState::sign(next, &self.owner_key)?;
        Ok(true)
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
        self.state = SignedState::sign(next, &self.owner_key)?;
        Ok(true)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        let persisted = PersistedState {
            schema_version: SCHEMA_VERSION,
            signed_state: self.state.clone(),
            redeemed_token_ids: self.used_tokens.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn random_node_key() -> [u8; 32] {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        key
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(loaded.verifying_key(), store.owner_key.verifying_key());
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
}
