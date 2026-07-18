use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LocalConfig {
    pub management_token: Option<String>,
    pub api_key: Option<String>,
}

impl LocalConfig {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid local config {}: {error}", path.display()),
            )
        })
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn generate_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn resolve_token_in(
    env_value: Option<String>,
    path: &Path,
    get: impl Fn(&LocalConfig) -> Option<String>,
    set: impl Fn(&mut LocalConfig, String),
) -> std::io::Result<(String, bool)> {
    if let Some(value) = env_value {
        return Ok((value, false));
    }
    let mut config = LocalConfig::load(path)?;
    if let Some(value) = get(&config) {
        return Ok((value, false));
    }
    let generated = generate_token();
    set(&mut config, generated.clone());
    config.save(path)?;
    Ok((generated, true))
}

/// Resolves DLLMD_MANAGEMENT_TOKEN: environment variable, then the
/// persisted config file, then a freshly generated token (persisted back
/// to the config file). Returns the token and whether it was just
/// generated.
pub fn resolve_management_token() -> std::io::Result<(String, bool)> {
    let env_value = std::env::var("DLLMD_MANAGEMENT_TOKEN").ok();
    let path = crate::default_config_path()?;
    resolve_token_in(
        env_value,
        &path,
        |c| c.management_token.clone(),
        |c, v| c.management_token = Some(v),
    )
}

/// Same as `resolve_management_token`, for DLLMD_API_KEY.
pub fn resolve_api_key() -> std::io::Result<(String, bool)> {
    let env_value = std::env::var("DLLMD_API_KEY").ok();
    let path = crate::default_config_path()?;
    resolve_token_in(
        env_value,
        &path,
        |c| c.api_key.clone(),
        |c, v| c.api_key = Some(v),
    )
}

fn read_management_token_in(env_value: Option<String>, path: &Path) -> Option<String> {
    if let Some(value) = env_value {
        return Some(value);
    }
    LocalConfig::load(path).ok()?.management_token
}

/// Reads DLLMD_MANAGEMENT_TOKEN from the environment, then falls back to
/// the persisted config file. Never generates or writes anything -- used
/// by the CLI, which has no bootstrap step of its own.
pub fn read_management_token() -> Option<String> {
    let env_value = std::env::var("DLLMD_MANAGEMENT_TOKEN").ok();
    let path = crate::default_config_path().ok()?;
    read_management_token_in(env_value, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_config_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        (dir, path)
    }

    #[test]
    fn load_missing_file_returns_default() {
        let (_dir, path) = temp_config_path();
        let config = LocalConfig::load(&path).unwrap();
        assert!(config.management_token.is_none());
        assert!(config.api_key.is_none());
    }

    #[test]
    fn load_malformed_file_returns_invalid_data() {
        let (_dir, path) = temp_config_path();
        fs::write(&path, b"{not-json").unwrap();

        let error = LocalConfig::load(&path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("invalid local config"));
    }

    #[test]
    fn resolve_does_not_overwrite_malformed_file() {
        let (_dir, path) = temp_config_path();
        let malformed = b"{not-json";
        fs::write(&path, malformed).unwrap();

        let error = resolve_token_in(
            None,
            &path,
            |c| c.management_token.clone(),
            |c, v| c.management_token = Some(v),
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), malformed);
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, path) = temp_config_path();
        let config = LocalConfig {
            management_token: Some("mgmt-123".into()),
            api_key: Some("api-456".into()),
        };
        config.save(&path).unwrap();
        let loaded = LocalConfig::load(&path).unwrap();
        assert_eq!(loaded.management_token.as_deref(), Some("mgmt-123"));
        assert_eq!(loaded.api_key.as_deref(), Some("api-456"));
    }

    #[test]
    fn resolve_env_value_wins_and_does_not_touch_file() {
        let (_dir, path) = temp_config_path();
        let (value, generated) = resolve_token_in(
            Some("from-env".to_string()),
            &path,
            |c| c.management_token.clone(),
            |c, v| c.management_token = Some(v),
        )
        .unwrap();
        assert_eq!(value, "from-env");
        assert!(!generated);
        assert!(!path.exists());
    }

    #[test]
    fn resolve_existing_config_value_wins_over_generating() {
        let (_dir, path) = temp_config_path();
        let existing = LocalConfig {
            management_token: Some("from-file".into()),
            api_key: None,
        };
        existing.save(&path).unwrap();
        let (value, generated) = resolve_token_in(
            None,
            &path,
            |c| c.management_token.clone(),
            |c, v| c.management_token = Some(v),
        )
        .unwrap();
        assert_eq!(value, "from-file");
        assert!(!generated);
    }

    #[test]
    fn resolve_generates_and_persists_when_nothing_set() {
        let (_dir, path) = temp_config_path();
        let (value, generated) = resolve_token_in(
            None,
            &path,
            |c| c.management_token.clone(),
            |c, v| c.management_token = Some(v),
        )
        .unwrap();
        assert!(generated);
        assert_eq!(value.len(), 64);
        assert!(value.chars().all(|c| c.is_ascii_hexdigit()));
        let reloaded = LocalConfig::load(&path).unwrap();
        assert_eq!(reloaded.management_token.as_deref(), Some(value.as_str()));
    }

    #[test]
    fn resolve_second_call_reuses_generated_value() {
        let (_dir, path) = temp_config_path();
        let (first, _) = resolve_token_in(
            None,
            &path,
            |c| c.api_key.clone(),
            |c, v| c.api_key = Some(v),
        )
        .unwrap();
        let (second, generated_again) = resolve_token_in(
            None,
            &path,
            |c| c.api_key.clone(),
            |c, v| c.api_key = Some(v),
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(!generated_again);
    }

    #[test]
    fn read_management_token_in_prefers_env_over_file() {
        let (_dir, path) = temp_config_path();
        let existing = LocalConfig {
            management_token: Some("from-file".into()),
            api_key: None,
        };
        existing.save(&path).unwrap();
        let value = read_management_token_in(Some("from-env".to_string()), &path);
        assert_eq!(value.as_deref(), Some("from-env"));
    }

    #[test]
    fn read_management_token_in_falls_back_to_file() {
        let (_dir, path) = temp_config_path();
        let existing = LocalConfig {
            management_token: Some("from-file".into()),
            api_key: None,
        };
        existing.save(&path).unwrap();
        let value = read_management_token_in(None, &path);
        assert_eq!(value.as_deref(), Some("from-file"));
    }

    #[test]
    fn read_management_token_in_returns_none_when_nothing_set() {
        let (_dir, path) = temp_config_path();
        let value = read_management_token_in(None, &path);
        assert!(value.is_none());
    }
}
