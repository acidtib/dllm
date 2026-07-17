use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp_unix: u64,
    pub actor: String,
    pub action: String,
    pub target: Option<String>,
    pub outcome: String,
}

impl AuditEntry {
    fn to_json_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("AuditEntry is always serializable");
        line.push('\n');
        line
    }
}

pub struct AuditLog {
    sender: mpsc::UnboundedSender<AuditEntry>,
}

impl AuditLog {
    /// Create a new AuditLog, spawning a background writer task.
    /// `log_path` is the directory where audit files are stored.
    /// `max_bytes` is the rotation threshold.
    pub fn new(log_dir: PathBuf, max_bytes: u64) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<AuditEntry>();
        tokio::spawn(async move {
            let _ = run_writer(&mut receiver, log_dir, max_bytes).await;
        });
        Self { sender }
    }

    /// Enqueue an audit entry. Best-effort: if the writer task has died,
    /// the entry is silently dropped.
    pub fn log(&self, entry: AuditEntry) {
        let _ = self.sender.send(entry);
    }
}

async fn run_writer(
    receiver: &mut mpsc::UnboundedReceiver<AuditEntry>,
    log_dir: PathBuf,
    max_bytes: u64,
) -> std::io::Result<()> {
    fs::create_dir_all(&log_dir)?;
    let mut writer = open_current(&log_dir)?;
    let mut bytes_written: u64 = current_size(&log_dir);
    while let Some(entry) = receiver.recv().await {
        let line = entry.to_json_line();
        let line_len = line.len() as u64;
        if bytes_written + line_len > max_bytes {
            writer.flush()?;
            rotate(&log_dir)?;
            writer = open_current(&log_dir)?;
            bytes_written = current_size(&log_dir);
        }
        // Best-effort write: if it fails, drop the entry and continue.
        if writer.write_all(line.as_bytes()).is_ok() {
            let _ = writer.flush();
            bytes_written += line_len;
        }
    }
    Ok(())
}

fn current_path(log_dir: &std::path::Path) -> PathBuf {
    log_dir.join("audit.jsonl")
}

fn current_size(log_dir: &std::path::Path) -> u64 {
    fs::metadata(current_path(log_dir))
        .map(|meta| meta.len())
        .unwrap_or(0)
}

fn open_current(log_dir: &std::path::Path) -> std::io::Result<BufWriter<File>> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(current_path(log_dir))?;
    Ok(BufWriter::new(file))
}

fn rotate(log_dir: &std::path::Path) -> std::io::Result<()> {
    let current = current_path(log_dir);
    if !current.exists() {
        return Ok(());
    }
    // Find the next available rotated filename.
    for i in 1..u64::MAX {
        let rotated = log_dir.join(format!("audit.{}.jsonl", i));
        if !rotated.exists() {
            fs::rename(&current, &rotated)?;
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn log_writes_entries() {
        let dir = std::env::temp_dir().join(format!("dllm-audit-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = AuditLog::new(dir.clone(), 1024 * 1024);
        log.log(AuditEntry {
            timestamp_unix: 1000,
            actor: "admin".into(),
            action: "ban_node".into(),
            target: Some("deadbeef...".into()),
            outcome: "ok".into(),
        });
        log.log(AuditEntry {
            timestamp_unix: 1001,
            actor: "viewer".into(),
            action: "list_access_requests".into(),
            target: None,
            outcome: "ok".into(),
        });
        // Give the writer task time to flush.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let contents = fs::read_to_string(dir.join("audit.jsonl")).unwrap_or_default();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.action, "ban_node");
        assert_eq!(first.outcome, "ok");
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rotation_creates_new_file() {
        let dir = std::env::temp_dir().join(format!("dllm-audit-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // Small max_bytes so rotation triggers quickly.
        let log = AuditLog::new(dir.clone(), 100);
        for i in 0..10 {
            log.log(AuditEntry {
                timestamp_unix: i,
                actor: "test".into(),
                action: "test".into(),
                target: None,
                outcome: "ok".into(),
            });
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        // Should have rotated at least once.
        let rotated = dir.join("audit.1.jsonl");
        assert!(
            rotated.exists(),
            "expected rotated file audit.1.jsonl to exist"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dropped_sender_does_not_panic() {
        let dir = std::env::temp_dir().join(format!("dllm-audit-drop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (sender, _receiver) = mpsc::unbounded_channel::<AuditEntry>();
        let log = AuditLog { sender };
        // The receiver is dropped, so the writer task never started.
        // Logging should not panic.
        log.log(AuditEntry {
            timestamp_unix: 1,
            actor: "test".into(),
            action: "test".into(),
            target: None,
            outcome: "ok".into(),
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
