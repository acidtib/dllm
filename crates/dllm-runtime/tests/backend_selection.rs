//! Task 2 scenario coverage for universal-artifact extraction and selection:
//! missing libraries, corrupt cache replacement, concurrent extraction,
//! explicit-backend failure, and CPU fallback.

use dllm_runtime::backend::{
    select, Artifact, BackendCache, BackendKind, BackendOutcome, BackendProbe, DeviceInfo,
    PayloadSource, RejectReason, SelectionMode,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn artifact(file: &str, bytes: &[u8]) -> Artifact {
    Artifact {
        file: file.to_string(),
        sha256: sha256_hex(bytes),
    }
}

/// Probe driven by a fixed table of results, standing in for the real ggml
/// device probe. The selection state machine only depends on the trait.
struct MockProbe {
    results: HashMap<BackendKind, Result<Vec<DeviceInfo>, RejectReason>>,
}

impl MockProbe {
    fn new() -> Self {
        Self {
            results: HashMap::new(),
        }
    }

    fn with(mut self, kind: BackendKind, result: Result<Vec<DeviceInfo>, RejectReason>) -> Self {
        self.results.insert(kind, result);
        self
    }

    fn device(kind: BackendKind, name: &str) -> Vec<DeviceInfo> {
        vec![DeviceInfo {
            backend: kind,
            index: 0,
            name: name.to_string(),
        }]
    }
}

impl BackendProbe for MockProbe {
    fn probe(&self, kind: BackendKind, _cache_dir: &Path) -> Result<Vec<DeviceInfo>, RejectReason> {
        self.results
            .get(&kind)
            .cloned()
            .unwrap_or(Err(RejectReason::LoadFailed("no result configured".into())))
    }
}

#[test]
fn forced_backend_without_payload_reports_missing() {
    let cache = tempfile::tempdir().unwrap();
    let probe = MockProbe::new().with(
        BackendKind::Cpu,
        Ok(MockProbe::device(BackendKind::Cpu, "host cpu")),
    );

    let result = select(
        SelectionMode::Forced(BackendKind::Cuda),
        &[BackendKind::Cpu],
        cache.path(),
        &probe,
    );

    assert_eq!(result.selected, None);
    assert_eq!(result.forced, Some(BackendKind::Cuda));
    assert_eq!(
        result.report(BackendKind::Cuda).unwrap().outcome,
        BackendOutcome::Rejected(RejectReason::PayloadMissing)
    );
    // A forced backend never falls back to CPU even when CPU would work.
    assert!(result.report(BackendKind::Cpu).is_none());
}

#[test]
fn auto_missing_backend_load_falls_through() {
    let cache = tempfile::tempdir().unwrap();
    let probe = MockProbe::new()
        .with(
            BackendKind::Cuda,
            Err(RejectReason::LoadFailed("libcuda.so.1 not found".into())),
        )
        .with(
            BackendKind::Cpu,
            Ok(MockProbe::device(BackendKind::Cpu, "host cpu")),
        );

    let result = select(
        SelectionMode::Auto,
        &[BackendKind::Cuda, BackendKind::Cpu],
        cache.path(),
        &probe,
    );

    assert_eq!(result.selected, Some(BackendKind::Cpu));
    assert!(matches!(
        result.report(BackendKind::Cuda).unwrap().outcome,
        BackendOutcome::Rejected(RejectReason::LoadFailed(_))
    ));
}

#[test]
fn corrupt_cache_file_is_replaced() {
    let base = tempfile::tempdir().unwrap();
    let cache = BackendCache::open(base.path(), "v1").unwrap();
    let bytes = b"the real backend object bytes";
    let art = artifact("libggml-cpu-haswell.so", bytes);

    let path = cache.ensure(&art, PayloadSource::Embedded(bytes)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // Simulate a truncated or tampered cache entry. Files are read-only, so
    // restore write access before overwriting, mirroring an external actor.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o600);
    }
    let _ = &mut perms;
    std::fs::set_permissions(&path, perms).unwrap();
    std::fs::write(&path, b"corrupted").unwrap();

    let repaired = cache.ensure(&art, PayloadSource::Embedded(bytes)).unwrap();
    assert_eq!(std::fs::read(repaired).unwrap(), bytes);
}

#[test]
fn checksum_mismatch_is_rejected() {
    let base = tempfile::tempdir().unwrap();
    let cache = BackendCache::open(base.path(), "v1").unwrap();
    // Declared checksum does not match the supplied bytes.
    let lying = Artifact {
        file: "libggml-vulkan.so".to_string(),
        sha256: sha256_hex(b"expected"),
    };

    let err = cache
        .ensure(&lying, PayloadSource::Embedded(b"actual different bytes"))
        .unwrap_err();
    assert!(
        matches!(err, dllm_runtime::backend::CacheError::Checksum { .. }),
        "expected checksum error, got {err:?}"
    );
    // Nothing partial is left behind.
    assert!(!cache.dir().join("libggml-vulkan.so").exists());
}

#[test]
fn concurrent_extraction_publishes_one_valid_file() {
    let base = tempfile::tempdir().unwrap();
    let cache = Arc::new(BackendCache::open(base.path(), "v1").unwrap());
    let bytes: Vec<u8> = (0..64 * 1024 + 7).map(|i| (i % 251) as u8).collect();
    let art = Arc::new(artifact("libggml-cuda.so", &bytes));
    let bytes = Arc::new(bytes);

    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let art = Arc::clone(&art);
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                cache.ensure(&art, PayloadSource::Embedded(&bytes)).unwrap()
            })
        })
        .collect();

    for handle in handles {
        let path = handle.join().unwrap();
        assert_eq!(&std::fs::read(path).unwrap(), bytes.as_ref());
    }
    // No stray temp files survive a successful extraction.
    let leftovers: Vec<_> = std::fs::read_dir(cache.dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp."))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

#[test]
fn explicit_backend_failure_does_not_fall_back() {
    let cache = tempfile::tempdir().unwrap();
    let probe = MockProbe::new()
        .with(
            BackendKind::Cuda,
            Err(RejectReason::Unsupported(
                "compute capability 6.1 < 7.5".into(),
            )),
        )
        .with(
            BackendKind::Cpu,
            Ok(MockProbe::device(BackendKind::Cpu, "host cpu")),
        );

    let result = select(
        SelectionMode::Forced(BackendKind::Cuda),
        &[BackendKind::Cuda, BackendKind::Cpu],
        cache.path(),
        &probe,
    );

    assert_eq!(result.selected, None);
    assert!(matches!(
        result.report(BackendKind::Cuda).unwrap().outcome,
        BackendOutcome::Rejected(RejectReason::Unsupported(_))
    ));
    // Forced mode considers only the requested backend.
    assert_eq!(result.reports.len(), 1);
}

#[test]
fn auto_falls_back_to_cpu_after_accelerators_rejected() {
    let cache = tempfile::tempdir().unwrap();
    let probe = MockProbe::new()
        .with(
            BackendKind::Cuda,
            Err(RejectReason::Unsupported("no supported CUDA device".into())),
        )
        .with(
            BackendKind::Vulkan,
            Err(RejectReason::LoadFailed("libvulkan.so.1 not found".into())),
        )
        .with(
            BackendKind::Cpu,
            Ok(MockProbe::device(BackendKind::Cpu, "host cpu")),
        );

    let result = select(
        SelectionMode::Auto,
        &[BackendKind::Cuda, BackendKind::Vulkan, BackendKind::Cpu],
        cache.path(),
        &probe,
    );

    // On non-macOS the order is cuda, vulkan, cpu. On macOS neither cuda nor
    // vulkan is a platform candidate, so cpu is selected directly. Either way
    // the selected backend is cpu.
    assert_eq!(result.selected, Some(BackendKind::Cpu));
    assert_eq!(
        result.report(BackendKind::Cpu).unwrap().outcome,
        BackendOutcome::Selected
    );
    #[cfg(not(target_os = "macos"))]
    {
        assert!(matches!(
            result.report(BackendKind::Cuda).unwrap().outcome,
            BackendOutcome::Rejected(RejectReason::Unsupported(_))
        ));
        assert!(matches!(
            result.report(BackendKind::Vulkan).unwrap().outcome,
            BackendOutcome::Rejected(RejectReason::LoadFailed(_))
        ));
    }
}

#[test]
fn backend_with_zero_devices_is_rejected() {
    let cache = tempfile::tempdir().unwrap();
    let probe = MockProbe::new()
        .with(BackendKind::Cuda, Ok(Vec::new()))
        .with(
            BackendKind::Cpu,
            Ok(MockProbe::device(BackendKind::Cpu, "host cpu")),
        );

    let result = select(
        SelectionMode::Auto,
        &[BackendKind::Cuda, BackendKind::Cpu],
        cache.path(),
        &probe,
    );

    assert_eq!(result.selected, Some(BackendKind::Cpu));
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        result.report(BackendKind::Cuda).unwrap().outcome,
        BackendOutcome::Rejected(RejectReason::NoDevices)
    );
}
