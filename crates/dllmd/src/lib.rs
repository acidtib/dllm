use dllm_protocol::{JoinToken, Member, NetworkState, SignedState, StateError, SCHEMA_VERSION};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use std::{collections::HashSet, fs, path::Path};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("state storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("state encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("join token belongs to another network")]
    WrongNetwork,
    #[error("join token has already been redeemed")]
    TokenUsed,
}

pub struct NetworkStore {
    pub owner_key: SigningKey,
    pub state: SignedState,
    used_tokens: HashSet<Uuid>,
}

impl NetworkStore {
    pub fn create(name: impl Into<String>) -> Self {
        let owner_key = SigningKey::generate(&mut rand::thread_rng());
        let state = NetworkState {
            schema_version: SCHEMA_VERSION,
            network_id: Uuid::new_v4(),
            name: name.into(),
            owner_pubkey: owner_key.verifying_key().to_bytes(),
            generation: 1,
            members: Vec::new(),
        };
        let signed = SignedState::sign(state, &owner_key).expect("new owner state is valid");
        Self { owner_key, state: signed, used_tokens: HashSet::new() }
    }

    pub fn issue_join_token(&self, expires_at_unix: Option<u64>) -> JoinToken {
        JoinToken::new(self.state.state.network_id, self.state.state.owner_pubkey, expires_at_unix)
    }

    pub fn redeem_join_token(&mut self, token: JoinToken, node_pubkey: [u8; 32]) -> Result<(), StoreError> {
        if token.network_id != self.state.state.network_id || token.owner_pubkey != self.state.state.owner_pubkey {
            return Err(StoreError::WrongNetwork);
        }
        if !self.used_tokens.insert(token.token_id) {
            return Err(StoreError::TokenUsed);
        }
        let mut next = self.state.state.clone();
        next.generation += 1;
        next.members.push(Member { node_pubkey, joined_generation: next.generation });
        self.state = SignedState::sign(next, &self.owner_key)?;
        Ok(())
    }

    pub fn revoke_member(&mut self, node_pubkey: [u8; 32]) -> Result<bool, StoreError> {
        let mut next = self.state.state.clone();
        let before = next.members.len();
        next.members.retain(|member| member.node_pubkey != node_pubkey);
        if next.members.len() == before {
            return Ok(false);
        }
        next.generation += 1;
        self.state = SignedState::sign(next, &self.owner_key)?;
        Ok(true)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(&self.state)?;
        fs::write(path, bytes)?;
        Ok(())
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

    #[test]
    fn redemption_advances_signed_generation_and_is_single_use() {
        let mut store = NetworkStore::create("test");
        let token = store.issue_join_token(None);
        let node = NetworkStore::random_node_key();
        store.redeem_join_token(token.clone(), node).unwrap();
        assert_eq!(store.state.state.generation, 2);
        assert_eq!(store.state.state.members.len(), 1);
        assert!(store.state.verify().is_ok());
        assert!(matches!(store.redeem_join_token(token, NetworkStore::random_node_key()), Err(StoreError::TokenUsed)));
    }

    #[test]
    fn revocation_advances_generation_and_is_idempotent() {
        let mut store = NetworkStore::create("test");
        let node = NetworkStore::random_node_key();
        store.redeem_join_token(store.issue_join_token(None), node).unwrap();
        assert!(store.revoke_member(node).unwrap());
        assert_eq!(store.state.state.generation, 3);
        assert!(store.state.state.members.is_empty());
        assert!(!store.revoke_member(node).unwrap());
        assert!(store.state.verify().is_ok());
    }
}
