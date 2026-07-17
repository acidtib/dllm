use std::collections::HashMap;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    pub max_requests: u32,
    pub window_seconds: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window_seconds: 60,
        }
    }
}

struct WindowState {
    timestamps: Vec<u64>,
    config: RateLimitConfig,
}

pub struct RateLimiter<K: std::hash::Hash + Eq + Clone> {
    windows: Mutex<HashMap<K, WindowState>>,
}

impl<K: std::hash::Hash + Eq + Clone> Default for RateLimiter<K> {
    fn default() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> RateLimiter<K> {
    pub fn new() -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether `key` is within its rate limit, recording the attempt.
    /// Returns true if admitted, false if the window is exhausted.
    pub async fn check(&self, key: &K, now_unix: u64, config: &RateLimitConfig) -> bool {
        let mut windows = self.windows.lock().await;
        let state = windows.entry(key.clone()).or_insert_with(|| WindowState {
            timestamps: Vec::new(),
            config: config.clone(),
        });
        // Update config if it changed.
        state.config = config.clone();
        // Prune timestamps outside the window.
        let window_start = now_unix.saturating_sub(state.config.window_seconds as u64);
        state.timestamps.retain(|ts| *ts > window_start);
        if state.timestamps.len() >= state.config.max_requests as usize {
            return false;
        }
        state.timestamps.push(now_unix);
        true
    }

    /// Remove stale entries for keys not seen since `older_than_unix`.
    pub async fn prune(&self, older_than_unix: u64) {
        let mut windows = self.windows.lock().await;
        windows.retain(|_, state| {
            state.timestamps.retain(|ts| *ts > older_than_unix);
            !state.timestamps.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admits_up_to_max_requests() {
        let limiter = RateLimiter::<String>::new();
        let config = RateLimitConfig {
            max_requests: 3,
            window_seconds: 3600,
        };
        let key = "192.168.1.1".to_string();
        assert!(limiter.check(&key, 1000, &config).await);
        assert!(limiter.check(&key, 1001, &config).await);
        assert!(limiter.check(&key, 1002, &config).await);
        assert!(!limiter.check(&key, 1003, &config).await);
    }

    #[tokio::test]
    async fn window_rollover_admits_again() {
        let limiter = RateLimiter::<String>::new();
        let config = RateLimitConfig {
            max_requests: 2,
            window_seconds: 60,
        };
        let key = "10.0.0.1".to_string();
        assert!(limiter.check(&key, 1000, &config).await);
        assert!(limiter.check(&key, 1001, &config).await);
        assert!(!limiter.check(&key, 1002, &config).await);
        // Jump past the window.
        assert!(limiter.check(&key, 1061, &config).await);
    }

    #[tokio::test]
    async fn isolated_keys() {
        let limiter = RateLimiter::<String>::new();
        let config = RateLimitConfig {
            max_requests: 1,
            window_seconds: 3600,
        };
        let key_a = "a".to_string();
        let key_b = "b".to_string();
        assert!(limiter.check(&key_a, 1000, &config).await);
        assert!(!limiter.check(&key_a, 1001, &config).await);
        assert!(limiter.check(&key_b, 1002, &config).await);
    }

    #[tokio::test]
    async fn prune_removes_stale_keys() {
        let limiter = RateLimiter::<String>::new();
        let config = RateLimitConfig {
            max_requests: 1,
            window_seconds: 60,
        };
        let key = "stale".to_string();
        assert!(limiter.check(&key, 1000, &config).await);
        // Prune with a cutoff well past the window.
        limiter.prune(2000).await;
        // The key should be gone; a new check creates a fresh entry.
        assert!(limiter.check(&key, 3000, &config).await);
    }
}
