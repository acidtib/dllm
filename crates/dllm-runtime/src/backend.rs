//! Universal-artifact backend extraction and selection.
//!
//! A single native artifact ships every ggml backend object it might use
//! (CPU variants plus CUDA/Vulkan/Metal where applicable) and picks one at
//! runtime. This module owns three concerns:
//!
//! 1. Parsing the `DLLMD_BACKEND` override into a [`SelectionMode`].
//! 2. Extracting versioned backend payloads into a private, per-version cache
//!    directory (temp file, checksum verification, atomic rename, restrictive
//!    permissions), so backends load from an explicit path with no
//!    `LD_LIBRARY_PATH` or global loader change.
//! 3. Selecting a backend in platform order, validating device capability
//!    before committing, and falling back only in automatic mode.
//!
//! Capability probing (loading a backend object, enumerating devices, and
//! confirming the device can actually run compute) is expressed through the
//! [`BackendProbe`] trait. The real probe that calls into the ggml dynamic
//! loader lands with the embedded runtime; keeping it behind a trait lets the
//! selection state machine be exercised without a GPU. The capability check is
//! required, not optional: an incompatible CUDA backend loads and enumerates
//! devices successfully and then aborts the process natively on first compute,
//! so selection must reject it before any work is dispatched.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Environment variable that overrides automatic backend selection.
pub const BACKEND_ENV: &str = "DLLMD_BACKEND";

/// Accelerator family a set of ggml dynamic backend objects belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    Cuda,
    Vulkan,
    Metal,
    Cpu,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Cuda => "cuda",
            BackendKind::Vulkan => "vulkan",
            BackendKind::Metal => "metal",
            BackendKind::Cpu => "cpu",
        }
    }

    /// Parses a case-insensitive backend name. Returns `None` for anything
    /// that is not a known backend (including `auto`, which is not a backend).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cuda" => Some(BackendKind::Cuda),
            "vulkan" => Some(BackendKind::Vulkan),
            "metal" => Some(BackendKind::Metal),
            "cpu" => Some(BackendKind::Cpu),
            _ => None,
        }
    }
}

/// The automatic candidate order for the current platform, highest preference
/// first. CPU is always last so it acts as the universal fallback. Metal only
/// appears on macOS; CUDA and Vulkan only off macOS.
pub fn platform_auto_order() -> &'static [BackendKind] {
    #[cfg(target_os = "macos")]
    {
        &[BackendKind::Metal, BackendKind::Cpu]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[BackendKind::Cuda, BackendKind::Vulkan, BackendKind::Cpu]
    }
}

/// How discovery should choose a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    /// Try backends in platform order and fall back on the first that works.
    Auto,
    /// Use exactly this backend and do not fall back.
    Forced(BackendKind),
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown {BACKEND_ENV} value '{0}', expected auto, cuda, vulkan, metal, or cpu")]
pub struct SelectionModeParseError(String);

impl SelectionMode {
    /// Parses `auto|cuda|vulkan|metal|cpu` (case-insensitive). An empty or
    /// whitespace-only value is treated as `auto`.
    pub fn parse(value: &str) -> Result<Self, SelectionModeParseError> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
            return Ok(SelectionMode::Auto);
        }
        BackendKind::parse(trimmed)
            .map(SelectionMode::Forced)
            .ok_or_else(|| SelectionModeParseError(value.to_string()))
    }

    /// Reads [`BACKEND_ENV`]. An unset variable means [`SelectionMode::Auto`].
    /// A set but invalid value is an error rather than a silent fallback, so a
    /// typo does not quietly downgrade to CPU.
    pub fn from_env() -> Result<Self, SelectionModeParseError> {
        match std::env::var(BACKEND_ENV) {
            Ok(value) => Self::parse(&value),
            Err(_) => Ok(SelectionMode::Auto),
        }
    }
}

/// One packaged file: a backend shared object or a core library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// File name as it appears in the cache directory, e.g.
    /// `libggml-cuda.so`.
    pub file: String,
    /// Lowercase hex SHA-256 of the file contents.
    pub sha256: String,
}

/// The packaged shared objects for one backend family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendArtifacts {
    pub kind: BackendKind,
    pub files: Vec<Artifact>,
}

/// Manifest describing the backend payloads embedded in a native artifact.
///
/// `core` lists the always-loaded libraries (`libggml-base`, `libggml`,
/// `libllama`, and their unconditional dependencies). `backends` lists the
/// per-family objects that are extracted only when their family is selected.
/// `version` keys the private cache directory so a new artifact never reuses a
/// stale cache.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeManifest {
    pub version: String,
    pub core: Vec<Artifact>,
    pub backends: Vec<BackendArtifacts>,
}

impl RuntimeManifest {
    /// Backend families this artifact carries payloads for, in manifest order.
    pub fn available_kinds(&self) -> Vec<BackendKind> {
        self.backends.iter().map(|b| b.kind).collect()
    }

    pub fn backend(&self, kind: BackendKind) -> Option<&BackendArtifacts> {
        self.backends.iter().find(|b| b.kind == kind)
    }
}

/// Where the bytes for an [`Artifact`] come from during extraction.
///
/// `Embedded` is the native-artifact path (`include_bytes!`). `Path` is the
/// staged path used by the packaging script and tests, and avoids loading a
/// large backend object fully into memory.
pub enum PayloadSource<'a> {
    Embedded(&'a [u8]),
    Path(&'a Path),
}

impl<'a> PayloadSource<'a> {
    fn reader(&self) -> io::Result<Box<dyn Read + 'a>> {
        match self {
            PayloadSource::Embedded(bytes) => Ok(Box::new(io::Cursor::new(*bytes))),
            PayloadSource::Path(path) => Ok(Box::new(File::open(path)?)),
        }
    }
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("io error for {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        file: String,
        expected: String,
        actual: String,
    },
}

/// A private, per-version directory that backend objects are extracted into
/// and loaded from. The directory is created with owner-only permissions.
pub struct BackendCache {
    dir: PathBuf,
}

impl BackendCache {
    /// Opens (creating if needed) `base/<version>`. The version subdirectory
    /// isolates one artifact build's backends from another's, so upgrading the
    /// binary never loads a stale object.
    pub fn open(base: &Path, version: &str) -> Result<Self, CacheError> {
        let dir = base.join(version);
        fs::create_dir_all(&dir).map_err(|source| CacheError::Io {
            path: dir.clone(),
            source,
        })?;
        restrict_dir(&dir).map_err(|source| CacheError::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Ensures `artifact.file` is present in the cache with matching contents,
    /// returning its full path.
    ///
    /// Idempotent and safe under concurrent callers: if the file already
    /// exists with the expected checksum it is left untouched; otherwise the
    /// bytes are streamed into a unique temp file (verified against the
    /// expected checksum), made read-only, and atomically renamed into place.
    /// A corrupt or truncated existing file fails the checksum and is
    /// replaced. Concurrent extractors each write their own temp file, so the
    /// final rename always publishes a complete, verified object.
    pub fn ensure(
        &self,
        artifact: &Artifact,
        source: PayloadSource,
    ) -> Result<PathBuf, CacheError> {
        let target = self.dir.join(&artifact.file);
        if let Ok(existing) = file_sha256(&target) {
            if existing == artifact.sha256 {
                return Ok(target);
            }
        }

        let temp = self.temp_path(&artifact.file);
        let write_result = write_hashed(&temp, source);
        let actual = match write_result {
            Ok(actual) => actual,
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
        };
        if actual != artifact.sha256 {
            let _ = fs::remove_file(&temp);
            return Err(CacheError::Checksum {
                file: artifact.file.clone(),
                expected: artifact.sha256.clone(),
                actual,
            });
        }

        fs::rename(&temp, &target).map_err(|source| {
            let _ = fs::remove_file(&temp);
            CacheError::Io {
                path: target.clone(),
                source,
            }
        })?;
        Ok(target)
    }

    /// Extracts the core libraries plus the payloads for `kind`. Resolving the
    /// bytes for each [`Artifact`] is delegated to `resolve` so both the
    /// embedded and staged sources work through one path.
    pub fn extract<'a, F>(
        &self,
        manifest: &'a RuntimeManifest,
        kind: BackendKind,
        mut resolve: F,
    ) -> Result<Vec<PathBuf>, CacheError>
    where
        F: FnMut(&'a Artifact) -> PayloadSource<'a>,
    {
        let backend = manifest.backend(kind);
        let backend_files = backend.into_iter().flat_map(|b| b.files.iter());
        let mut extracted = Vec::new();
        for artifact in manifest.core.iter().chain(backend_files) {
            extracted.push(self.ensure(artifact, resolve(artifact))?);
        }
        Ok(extracted)
    }

    fn temp_path(&self, file: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(format!(".{file}.tmp.{}.{seq}", std::process::id()))
    }
}

/// Streams `source` into `temp`, returning the SHA-256 of the bytes written.
/// The caller compares it against the expected checksum.
fn write_hashed(temp: &Path, source: PayloadSource) -> Result<String, CacheError> {
    let io_err = |source: io::Error| CacheError::Io {
        path: temp.to_path_buf(),
        source,
    };
    let mut reader = source.reader().map_err(io_err)?;
    let mut file = File::create(temp).map_err(io_err)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(io_err)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read]).map_err(io_err)?;
    }
    file.sync_all().map_err(io_err)?;
    restrict_file(&file).map_err(io_err)?;
    Ok(hex::encode(hasher.finalize()))
}

fn file_sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(unix)]
fn restrict_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Owner read+execute only: dlopen needs both, nothing else needs access.
    file.set_permissions(fs::Permissions::from_mode(0o500))
}

#[cfg(not(unix))]
fn restrict_file(_file: &File) -> io::Result<()> {
    Ok(())
}

/// A device exposed by a loaded backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub backend: BackendKind,
    pub index: u32,
    pub name: String,
}

/// Why a backend was not selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum RejectReason {
    /// No payload for this family is packaged for this platform.
    PayloadMissing,
    /// The backend object failed to load from the cache.
    LoadFailed(String),
    /// The backend loaded but enumerated no usable devices.
    NoDevices,
    /// Devices were present but none satisfied the capability check. This is
    /// the case that must be caught before dispatch: an incompatible CUDA
    /// backend enumerates devices and then aborts natively on first compute.
    Unsupported(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BackendOutcome {
    Selected,
    Rejected(RejectReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackendReport {
    pub kind: BackendKind,
    pub devices: Vec<DeviceInfo>,
    pub outcome: BackendOutcome,
}

/// The result of a discovery pass: which backend was chosen (if any) and, for
/// every backend considered, its devices and why it was accepted or rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveryResult {
    pub forced: Option<BackendKind>,
    pub selected: Option<BackendKind>,
    pub reports: Vec<BackendReport>,
}

impl DiscoveryResult {
    pub fn report(&self, kind: BackendKind) -> Option<&BackendReport> {
        self.reports.iter().find(|r| r.kind == kind)
    }
}

/// Loads a backend from the cache directory, enumerates its devices, and
/// validates that at least one device can actually run compute.
///
/// Implementations must not report success on enumeration alone. Returning
/// `Ok` with a non-empty device list means the backend is safe to dispatch to.
pub trait BackendProbe {
    fn probe(&self, kind: BackendKind, cache_dir: &Path) -> Result<Vec<DeviceInfo>, RejectReason>;
}

/// Runs discovery: builds the candidate order from `mode`, probes each
/// candidate against `cache_dir`, and returns the first backend that passes.
///
/// In [`SelectionMode::Auto`] the candidates are [`platform_auto_order`]
/// restricted to `available`, and a rejected candidate falls through to the
/// next. In [`SelectionMode::Forced`] the single candidate is the requested
/// backend with no fallback, and a forced backend that is not in `available`
/// is reported as [`RejectReason::PayloadMissing`].
pub fn select(
    mode: SelectionMode,
    available: &[BackendKind],
    cache_dir: &Path,
    probe: &dyn BackendProbe,
) -> DiscoveryResult {
    let forced = match mode {
        SelectionMode::Forced(kind) => Some(kind),
        SelectionMode::Auto => None,
    };
    let candidates: Vec<BackendKind> = match mode {
        SelectionMode::Forced(kind) => vec![kind],
        SelectionMode::Auto => platform_auto_order()
            .iter()
            .copied()
            .filter(|kind| available.contains(kind))
            .collect(),
    };

    let mut reports = Vec::new();
    let mut selected = None;
    for kind in candidates {
        if !available.contains(&kind) {
            reports.push(BackendReport {
                kind,
                devices: Vec::new(),
                outcome: BackendOutcome::Rejected(RejectReason::PayloadMissing),
            });
            continue;
        }
        match probe.probe(kind, cache_dir) {
            Ok(devices) if !devices.is_empty() => {
                reports.push(BackendReport {
                    kind,
                    devices,
                    outcome: BackendOutcome::Selected,
                });
                selected = Some(kind);
                break;
            }
            Ok(_) => reports.push(BackendReport {
                kind,
                devices: Vec::new(),
                outcome: BackendOutcome::Rejected(RejectReason::NoDevices),
            }),
            Err(reason) => reports.push(BackendReport {
                kind,
                devices: Vec::new(),
                outcome: BackendOutcome::Rejected(reason),
            }),
        }
    }

    DiscoveryResult {
        forced,
        selected,
        reports,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_parses_case_insensitively() {
        assert_eq!(BackendKind::parse("CUDA"), Some(BackendKind::Cuda));
        assert_eq!(BackendKind::parse(" vulkan "), Some(BackendKind::Vulkan));
        assert_eq!(BackendKind::parse("Metal"), Some(BackendKind::Metal));
        assert_eq!(BackendKind::parse("cpu"), Some(BackendKind::Cpu));
        assert_eq!(BackendKind::parse("auto"), None);
        assert_eq!(BackendKind::parse("rocm"), None);
    }

    #[test]
    fn selection_mode_parses_auto_and_forced() {
        assert_eq!(SelectionMode::parse("auto").unwrap(), SelectionMode::Auto);
        assert_eq!(SelectionMode::parse("").unwrap(), SelectionMode::Auto);
        assert_eq!(SelectionMode::parse("  ").unwrap(), SelectionMode::Auto);
        assert_eq!(
            SelectionMode::parse("cuda").unwrap(),
            SelectionMode::Forced(BackendKind::Cuda)
        );
        assert_eq!(
            SelectionMode::parse("nonsense").unwrap_err(),
            SelectionModeParseError("nonsense".to_string())
        );
    }

    #[test]
    fn platform_order_ends_with_cpu() {
        let order = platform_auto_order();
        assert_eq!(order.last(), Some(&BackendKind::Cpu));
    }

    #[test]
    fn manifest_round_trips_and_lists_kinds() {
        let json = r#"{
            "version": "gd-abc-llama-0.4.2",
            "core": [{ "file": "libllama.so", "sha256": "aa" }],
            "backends": [
                { "kind": "cuda", "files": [{ "file": "libggml-cuda.so", "sha256": "bb" }] },
                { "kind": "cpu", "files": [{ "file": "libggml-cpu-haswell.so", "sha256": "cc" }] }
            ]
        }"#;
        let manifest: RuntimeManifest = serde_json::from_str(json).unwrap();
        assert_eq!(
            manifest.available_kinds(),
            vec![BackendKind::Cuda, BackendKind::Cpu]
        );
        assert!(manifest.backend(BackendKind::Vulkan).is_none());
        let round = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            serde_json::from_str::<RuntimeManifest>(&round).unwrap(),
            manifest
        );
    }
}
