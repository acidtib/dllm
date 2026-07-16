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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub node_pubkey: [u8; 32],
    pub joined_generation: u64,
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
        let signature = Signature::from_slice(&self.signature)
            .map_err(|_| StateError::InvalidSignature)?;
        key.verify(&bytes, &signature)
            .map_err(|_| StateError::InvalidSignature)
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
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JoinToken {
    pub schema_version: u32,
    pub network_id: Uuid,
    pub token_id: Uuid,
    pub owner_pubkey: [u8; 32],
    pub expires_at_unix: Option<u64>,
    pub single_use: bool,
}

impl JoinToken {
    pub fn new(network_id: Uuid, owner_pubkey: [u8; 32], expires_at_unix: Option<u64>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            network_id,
            token_id: Uuid::new_v4(),
            owner_pubkey,
            expires_at_unix,
            single_use: true,
        }
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
        assert_eq!(SignedState::sign(state(&key), &other), Err(StateError::OwnerMismatch));
    }

    #[test]
    fn join_tokens_are_scoped_and_single_use() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let token = JoinToken::new(Uuid::new_v4(), key.verifying_key().to_bytes(), None);
        assert_eq!(token.schema_version, SCHEMA_VERSION);
        assert!(token.single_use);
        assert_eq!(token.owner_pubkey, key.verifying_key().to_bytes());
    }
}
