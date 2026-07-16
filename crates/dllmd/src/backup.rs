use crate::PersistedState;
use argon2::Argon2;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use ed25519_dalek::SigningKey;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};
use thiserror::Error;

const BACKUP_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct BackupPayload {
    state: Vec<u8>,
    owner_key: Vec<u8>,
    credentials: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct BackupEnvelope {
    version: u32,
    salt: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("backup storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("backup encoding error: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("unsupported backup version")]
    Version,
    #[error("backup passphrase is empty")]
    EmptyPassphrase,
    #[error("backup authentication failed")]
    Authentication,
    #[error("backup key derivation failed")]
    KeyDerivation,
    #[error("backup owner key is invalid")]
    OwnerKey,
    #[error("backup owner key does not match signed state")]
    OwnerMismatch,
    #[error("backup signed state is invalid")]
    InvalidState,
}

pub fn create_backup(
    state_path: &Path,
    owner_key_path: &Path,
    credentials_path: Option<&Path>,
    output_path: &Path,
    passphrase: &[u8],
) -> Result<(), BackupError> {
    if passphrase.is_empty() {
        return Err(BackupError::EmptyPassphrase);
    }
    let payload = BackupPayload {
        state: fs::read(state_path)?,
        owner_key: fs::read(owner_key_path)?,
        credentials: credentials_path.map(fs::read).transpose()?,
    };
    validate_payload(&payload)?;
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 12];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut nonce);
    let key = derive_key(passphrase, &salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            serde_json::to_vec(&payload)?.as_ref(),
        )
        .map_err(|_| BackupError::Authentication)?;
    let envelope = BackupEnvelope {
        version: BACKUP_VERSION,
        salt: salt.to_vec(),
        nonce: nonce.to_vec(),
        ciphertext,
    };
    write_private(output_path, &serde_json::to_vec_pretty(&envelope)?)
}

pub fn restore_backup(
    input_path: &Path,
    state_path: &Path,
    owner_key_path: &Path,
    credentials_path: Option<&Path>,
    passphrase: &[u8],
) -> Result<(), BackupError> {
    if passphrase.is_empty() {
        return Err(BackupError::EmptyPassphrase);
    }
    let envelope: BackupEnvelope = serde_json::from_slice(&fs::read(input_path)?)?;
    if envelope.version != BACKUP_VERSION || envelope.salt.len() != 16 || envelope.nonce.len() != 12
    {
        return Err(BackupError::Version);
    }
    let key = derive_key(passphrase, &envelope.salt)?;
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&envelope.nonce),
            envelope.ciphertext.as_ref(),
        )
        .map_err(|_| BackupError::Authentication)?;
    let payload: BackupPayload = serde_json::from_slice(&plaintext)?;
    validate_payload(&payload)?;
    write_private(state_path, &payload.state)?;
    write_private(owner_key_path, &payload.owner_key)?;
    if let (Some(path), Some(credentials)) = (credentials_path, payload.credentials) {
        write_private(path, &credentials)?;
    }
    Ok(())
}

fn validate_payload(payload: &BackupPayload) -> Result<(), BackupError> {
    let persisted: PersistedState =
        serde_json::from_slice(&payload.state).map_err(|_| BackupError::InvalidState)?;
    persisted
        .signed_state
        .verify()
        .map_err(|_| BackupError::InvalidState)?;
    let owner_key: [u8; 32] = payload
        .owner_key
        .as_slice()
        .try_into()
        .map_err(|_| BackupError::OwnerKey)?;
    let owner_key = SigningKey::from_bytes(&owner_key);
    if owner_key.verifying_key().to_bytes() != persisted.signed_state.state.owner_pubkey {
        return Err(BackupError::OwnerMismatch);
    }
    Ok(())
}

fn derive_key(passphrase: &[u8], salt: &[u8]) -> Result<[u8; 32], BackupError> {
    let mut key = [0_u8; 32];
    Argon2::default()
        .hash_password_into(passphrase, salt, &mut key)
        .map_err(|_| BackupError::KeyDerivation)?;
    Ok(key)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NetworkStore;
    use uuid::Uuid;

    #[test]
    fn encrypted_backup_restores_verified_control_plane() {
        let directory = std::env::temp_dir().join(format!("dllm-backup-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let state = directory.join("state.json");
        let key = directory.join("owner.key");
        let credentials = directory.join("credentials.json");
        let archive = directory.join("backup.json");
        let restored_state = directory.join("restored-state.json");
        let restored_key = directory.join("restored-owner.key");
        let restored_credentials = directory.join("restored-credentials.json");

        let store = NetworkStore::create("backup-test");
        store.save(&state).unwrap();
        store.save_owner_key(&key).unwrap();
        fs::write(&credentials, b"credential-digests").unwrap();
        create_backup(
            &state,
            &key,
            Some(&credentials),
            &archive,
            b"correct horse battery staple",
        )
        .unwrap();
        assert!(matches!(
            restore_backup(
                &archive,
                &restored_state,
                &restored_key,
                Some(&restored_credentials),
                b"wrong passphrase"
            ),
            Err(BackupError::Authentication)
        ));
        restore_backup(
            &archive,
            &restored_state,
            &restored_key,
            Some(&restored_credentials),
            b"correct horse battery staple",
        )
        .unwrap();
        let restored = NetworkStore::load(&restored_state, &restored_key).unwrap();
        assert_eq!(
            restored.state.state.network_id,
            store.state.state.network_id
        );
        assert_eq!(
            fs::read(restored_credentials).unwrap(),
            b"credential-digests"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(archive).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(restored_key).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
