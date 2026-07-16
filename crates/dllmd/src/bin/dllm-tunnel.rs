use std::{process::ExitStatus, time::Duration};
use tokio::process::{Child, Command};

struct TunnelConfig {
    ssh_binary: String,
    ssh_target: String,
    remote_bind: String,
    local_endpoint: String,
    retry_delay: Duration,
}

impl TunnelConfig {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let retry_delay = Duration::from_millis(
            std::env::var("DLLM_TUNNEL_RETRY_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1_000),
        );
        Ok(Self {
            ssh_binary: std::env::var("DLLM_TUNNEL_SSH_BINARY").unwrap_or_else(|_| "ssh".into()),
            ssh_target: std::env::var("DLLM_TUNNEL_SSH_TARGET")?,
            remote_bind: std::env::var("DLLM_TUNNEL_REMOTE_BIND")
                .unwrap_or_else(|_| "127.0.0.1:17443".into()),
            local_endpoint: std::env::var("DLLM_TUNNEL_LOCAL_ENDPOINT")
                .unwrap_or_else(|_| "127.0.0.1:7444".into()),
            retry_delay,
        })
    }

    fn ssh_args(&self) -> Vec<String> {
        vec![
            "-N".into(),
            "-T".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "ServerAliveInterval=10".into(),
            "-o".into(),
            "ServerAliveCountMax=3".into(),
            "-R".into(),
            format!("{}:{}", self.remote_bind, self.local_endpoint),
            self.ssh_target.clone(),
        ]
    }

    fn spawn(&self) -> std::io::Result<Child> {
        Command::new(&self.ssh_binary)
            .args(self.ssh_args())
            .kill_on_drop(true)
            .spawn()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = TunnelConfig::from_env()?;
    loop {
        println!(
            "connecting reverse tunnel {} through {}",
            config.remote_bind, config.ssh_target
        );
        let result = match config.spawn() {
            Ok(mut child) => tokio::select! {
                result = child.wait() => result,
                _ = tokio::signal::ctrl_c() => {
                    child.kill().await?;
                    return Ok(());
                }
            },
            Err(error) => Err(error),
        };
        report_exit(result);
        tokio::select! {
            _ = tokio::time::sleep(config.retry_delay) => {},
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

fn report_exit(result: std::io::Result<ExitStatus>) {
    match result {
        Ok(status) => eprintln!("reverse tunnel exited with {status}, reconnecting"),
        Err(error) => eprintln!("reverse tunnel failed: {error}, reconnecting"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_forward_is_loopback_scoped_by_default() {
        let config = TunnelConfig {
            ssh_binary: "ssh".into(),
            ssh_target: "relay@example".into(),
            remote_bind: "127.0.0.1:17443".into(),
            local_endpoint: "127.0.0.1:7444".into(),
            retry_delay: Duration::from_secs(1),
        };

        let args = config.ssh_args();
        assert!(args
            .windows(2)
            .any(|pair| { pair == ["-R", "127.0.0.1:17443:127.0.0.1:7444"] }));
        assert_eq!(args.last().map(String::as_str), Some("relay@example"));
    }
}
