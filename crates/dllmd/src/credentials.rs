use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, fs, path::PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ManagementRole {
    Viewer,
    Operator,
    Admin,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManagementCredential {
    pub token: String,
    pub role: ManagementRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCredential {
    id: Uuid,
    label: String,
    role: ManagementRole,
    token_hash: Vec<u8>,
    created_at_unix: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub label: String,
    pub role: ManagementRole,
    pub created_at_unix: u64,
    pub revocable: bool,
}

#[derive(Debug, Serialize)]
pub struct CreatedCredential {
    pub credential: CredentialSummary,
    pub token: String,
}

struct CredentialEntry {
    stored: StoredCredential,
    persisted: bool,
}

pub struct CredentialRegistry {
    entries: Vec<CredentialEntry>,
    path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("credential encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("credential label must not be empty")]
    EmptyLabel,
    #[error("credential persistence is not configured")]
    PersistenceDisabled,
    #[error("credential not found or not revocable")]
    NotRevocable,
    #[error("the last admin credential cannot be revoked")]
    LastAdmin,
}

impl CredentialRegistry {
    pub fn load(
        configured: Vec<ManagementCredential>,
        legacy_token: Option<String>,
        path: Option<PathBuf>,
    ) -> Result<Self, CredentialError> {
        let mut configured_by_token = HashMap::new();
        for credential in configured {
            if credential.token.is_empty() {
                continue;
            }
            configured_by_token
                .entry(credential.token)
                .and_modify(|role: &mut ManagementRole| *role = (*role).max(credential.role))
                .or_insert(credential.role);
        }
        if let Some(token) = legacy_token.filter(|token| !token.is_empty()) {
            configured_by_token.insert(token, ManagementRole::Admin);
        }
        let mut configured = configured_by_token.into_iter().collect::<Vec<_>>();
        configured.sort_by(|left, right| left.0.cmp(&right.0));
        let mut entries = Vec::new();
        for (index, (token, role)) in configured.into_iter().enumerate() {
            entries.push(CredentialEntry {
                stored: StoredCredential {
                    id: Uuid::new_v4(),
                    label: format!("configured-{}", index + 1),
                    role,
                    token_hash: hash_token(&token),
                    created_at_unix: 0,
                },
                persisted: false,
            });
        }
        if let Some(path) = &path {
            if path.exists() {
                let stored: Vec<StoredCredential> = serde_json::from_slice(&fs::read(path)?)?;
                entries.extend(stored.into_iter().map(|stored| CredentialEntry {
                    stored,
                    persisted: true,
                }));
            }
        }
        Ok(Self { entries, path })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn authorize(&self, token: &str, required: ManagementRole) -> Option<bool> {
        let supplied = hash_token(token);
        self.entries
            .iter()
            .find(|entry| entry.stored.token_hash == supplied)
            .map(|entry| entry.stored.role >= required)
    }

    pub fn list(&self) -> Vec<CredentialSummary> {
        let mut credentials = self.entries.iter().map(summary).collect::<Vec<_>>();
        credentials.sort_by_key(|credential| credential.id);
        credentials
    }

    pub fn create(
        &mut self,
        label: String,
        role: ManagementRole,
        now_unix: u64,
    ) -> Result<CreatedCredential, CredentialError> {
        if self.path.is_none() {
            return Err(CredentialError::PersistenceDisabled);
        }
        if label.trim().is_empty() {
            return Err(CredentialError::EmptyLabel);
        }
        let mut secret = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        let token = hex(&secret);
        let entry = CredentialEntry {
            stored: StoredCredential {
                id: Uuid::new_v4(),
                label,
                role,
                token_hash: hash_token(&token),
                created_at_unix: now_unix,
            },
            persisted: true,
        };
        let credential = summary(&entry);
        self.entries.push(entry);
        self.save()?;
        Ok(CreatedCredential { credential, token })
    }

    pub fn revoke(&mut self, id: Uuid) -> Result<(), CredentialError> {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.stored.id == id && entry.persisted)
        else {
            return Err(CredentialError::NotRevocable);
        };
        if self.entries[index].stored.role == ManagementRole::Admin
            && self
                .entries
                .iter()
                .filter(|entry| entry.stored.role == ManagementRole::Admin)
                .count()
                == 1
        {
            return Err(CredentialError::LastAdmin);
        }
        self.entries.remove(index);
        self.save()
    }

    fn save(&self) -> Result<(), CredentialError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let stored = self
            .entries
            .iter()
            .filter(|entry| entry.persisted)
            .map(|entry| &entry.stored)
            .collect::<Vec<_>>();
        fs::write(path, serde_json::to_vec_pretty(&stored)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn summary(entry: &CredentialEntry) -> CredentialSummary {
    CredentialSummary {
        id: entry.stored.id,
        label: entry.stored.label.clone(),
        role: entry.stored.role,
        created_at_unix: entry.stored.created_at_unix,
        revocable: entry.persisted,
    }
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
