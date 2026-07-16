use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceCredential {
    pub label: String,
    pub token: String,
    pub max_in_flight: usize,
}

#[derive(Clone)]
pub struct InferenceIdentity {
    pub label: String,
    pub quota: Arc<Semaphore>,
}

struct Entry {
    token_hash: Vec<u8>,
    identity: InferenceIdentity,
}

pub struct InferenceRegistry {
    entries: Vec<Entry>,
    anonymous: InferenceIdentity,
}

impl InferenceRegistry {
    pub fn new(
        configured: Vec<InferenceCredential>,
        legacy_token: Option<String>,
        default_limit: usize,
    ) -> Self {
        let mut entries = configured
            .into_iter()
            .filter(|credential| !credential.token.is_empty() && credential.max_in_flight > 0)
            .map(|credential| Entry {
                token_hash: hash_token(&credential.token),
                identity: InferenceIdentity {
                    label: credential.label,
                    quota: Arc::new(Semaphore::new(credential.max_in_flight)),
                },
            })
            .collect::<Vec<_>>();
        if let Some(token) = legacy_token.filter(|token| !token.is_empty()) {
            entries.push(Entry {
                token_hash: hash_token(&token),
                identity: InferenceIdentity {
                    label: "legacy".into(),
                    quota: Arc::new(Semaphore::new(default_limit)),
                },
            });
        }
        Self {
            entries,
            anonymous: InferenceIdentity {
                label: "anonymous".into(),
                quota: Arc::new(Semaphore::new(default_limit)),
            },
        }
    }

    pub fn authenticate(&self, token: Option<&str>) -> Option<InferenceIdentity> {
        if self.entries.is_empty() {
            return Some(self.anonymous.clone());
        }
        let supplied = hash_token(token?);
        self.entries
            .iter()
            .find(|entry| entry.token_hash == supplied)
            .map(|entry| entry.identity.clone())
    }
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}
