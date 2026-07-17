use dllm_protocol::{now_unix, NetworkState};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, TryAcquireError};

#[derive(Debug, Clone, Error)]
pub enum BudgetError {
    #[error("no resource budget exists for this node")]
    NoBudget,
    #[error("max-in-flight limit reached")]
    Saturated,
    #[error("request window exhausted")]
    WindowExhausted,
}

struct MemberBudgetState {
    semaphore: Arc<Semaphore>,
    window_timestamps: Vec<u64>,
    max_requests_per_window: u32,
    window_seconds: u32,
}

pub struct BudgetEnforcer {
    budgets: Mutex<HashMap<[u8; 32], MemberBudgetState>>,
}

/// A permit that releases one in-flight slot when dropped.
pub struct BudgetPermit {
    _permit: OwnedSemaphorePermit,
}

impl Default for BudgetEnforcer {
    fn default() -> Self {
        Self::new()
    }
}

impl BudgetEnforcer {
    pub fn new() -> Self {
        Self {
            budgets: Mutex::new(HashMap::new()),
        }
    }

    /// Reconcile internal state with the signed NetworkState.
    /// Creates budgets for new entries, updates budgets whose parameters changed,
    /// and drops budgets for members that no longer have entries.
    pub async fn reconcile(&self, state: &NetworkState) {
        let mut budgets = self.budgets.lock().await;
        // Remove budgets for nodes no longer in resource_budgets.
        budgets.retain(|node_pubkey, _| {
            state
                .resource_budgets
                .iter()
                .any(|budget| &budget.node_pubkey == node_pubkey)
        });
        // Insert or update budgets.
        for budget in &state.resource_budgets {
            if let Some(existing) = budgets.get_mut(&budget.node_pubkey) {
                // Update window parameters if they changed.
                existing.max_requests_per_window = budget.max_requests_per_window;
                existing.window_seconds = budget.window_seconds;
                // Note: we cannot resize the semaphore, so we keep the old one.
                // A generation bump on the signed state signals the change.
            } else {
                budgets.insert(
                    budget.node_pubkey,
                    MemberBudgetState {
                        semaphore: Arc::new(Semaphore::new(budget.max_in_flight as usize)),
                        window_timestamps: Vec::new(),
                        max_requests_per_window: budget.max_requests_per_window,
                        window_seconds: budget.window_seconds,
                    },
                );
            }
        }
    }

    /// Try to admit a request for `node_pubkey`.
    /// Checks both max_in_flight (via semaphore) and max_requests_per_window (via sliding window).
    pub async fn try_admit(
        &self,
        state: &NetworkState,
        node_pubkey: &[u8; 32],
    ) -> Result<BudgetPermit, BudgetError> {
        // Look up the signed budget entry for window parameters.
        let budget_entry = state
            .resource_budgets
            .iter()
            .find(|budget| &budget.node_pubkey == node_pubkey)
            .ok_or(BudgetError::NoBudget)?;
        let now = now_unix();
        let mut budgets = self.budgets.lock().await;
        let member_state = budgets.get_mut(node_pubkey).ok_or(BudgetError::NoBudget)?;
        // Check sliding window.
        if member_state.max_requests_per_window > 0 {
            let window_start = now.saturating_sub(member_state.window_seconds as u64);
            member_state
                .window_timestamps
                .retain(|ts| *ts > window_start);
            if member_state.window_timestamps.len() >= member_state.max_requests_per_window as usize
            {
                return Err(BudgetError::WindowExhausted);
            }
        }
        // Check in-flight concurrency.
        let permit = member_state
            .semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|e| match e {
                TryAcquireError::Closed => BudgetError::NoBudget,
                TryAcquireError::NoPermits => BudgetError::Saturated,
            })?;
        // Record window timestamp after passing both checks.
        if member_state.max_requests_per_window > 0 {
            member_state.window_timestamps.push(now);
        }
        // Ensure window_seconds matches the signed state.
        member_state.window_seconds = budget_entry.window_seconds;
        Ok(BudgetPermit { _permit: permit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dllm_protocol::ResourceBudget;
    use uuid::Uuid;

    fn make_state(budgets: Vec<ResourceBudget>) -> NetworkState {
        NetworkState {
            schema_version: 1,
            network_id: Uuid::new_v4(),
            name: "test".into(),
            owner_pubkey: [1; 32],
            generation: 1,
            members: vec![],
            model_assignments: vec![],
            placements: vec![],
            hardware_profiles: vec![],
            transport_bindings: vec![],
            transport_revocations: vec![],
            forwarding_policy: vec![],
            resource_budgets: budgets,
        }
    }

    #[tokio::test]
    async fn no_budget_entry_returns_no_budget() {
        let enforcer = BudgetEnforcer::new();
        let state = make_state(vec![]);
        let node = [2; 32];
        let result = enforcer.try_admit(&state, &node).await;
        assert!(matches!(result, Err(BudgetError::NoBudget)));
    }

    #[tokio::test]
    async fn budget_permits_up_to_max_in_flight() {
        let node = [2; 32];
        let state = make_state(vec![ResourceBudget {
            node_pubkey: node,
            max_in_flight: 2,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        }]);
        let enforcer = BudgetEnforcer::new();
        enforcer.reconcile(&state).await;
        // First two admissions succeed.
        let p1 = enforcer.try_admit(&state, &node).await.unwrap();
        let p2 = enforcer.try_admit(&state, &node).await.unwrap();
        // Third fails with Saturated.
        let result = enforcer.try_admit(&state, &node).await;
        assert!(matches!(result, Err(BudgetError::Saturated)));
        drop(p1);
        // After dropping one, admission succeeds again.
        let _p3 = enforcer.try_admit(&state, &node).await.unwrap();
        drop(p2);
    }

    #[tokio::test]
    async fn sliding_window_enforces_max_requests_per_window() {
        let node = [2; 32];
        let state = make_state(vec![ResourceBudget {
            node_pubkey: node,
            max_in_flight: 100, // high enough not to be the bottleneck
            max_requests_per_window: 2,
            window_seconds: 3600,
            granted_generation: 1,
        }]);
        let enforcer = BudgetEnforcer::new();
        enforcer.reconcile(&state).await;
        // First two admissions succeed.
        let _p1 = enforcer.try_admit(&state, &node).await.unwrap();
        let _p2 = enforcer.try_admit(&state, &node).await.unwrap();
        // Third fails with WindowExhausted.
        let result = enforcer.try_admit(&state, &node).await;
        assert!(matches!(result, Err(BudgetError::WindowExhausted)));
    }

    #[tokio::test]
    async fn isolated_member_budgets() {
        let node_a = [2; 32];
        let node_b = [3; 32];
        let state = make_state(vec![
            ResourceBudget {
                node_pubkey: node_a,
                max_in_flight: 1,
                max_requests_per_window: 0,
                window_seconds: 0,
                granted_generation: 1,
            },
            ResourceBudget {
                node_pubkey: node_b,
                max_in_flight: 1,
                max_requests_per_window: 0,
                window_seconds: 0,
                granted_generation: 1,
            },
        ]);
        let enforcer = BudgetEnforcer::new();
        enforcer.reconcile(&state).await;
        // Both nodes can hold one permit each.
        let _pa = enforcer.try_admit(&state, &node_a).await.unwrap();
        let _pb = enforcer.try_admit(&state, &node_b).await.unwrap();
        // Node A's second attempt fails.
        let result = enforcer.try_admit(&state, &node_a).await;
        assert!(matches!(result, Err(BudgetError::Saturated)));
    }

    #[tokio::test]
    async fn reconcile_removes_stale_budgets() {
        let node = [2; 32];
        let state = make_state(vec![ResourceBudget {
            node_pubkey: node,
            max_in_flight: 1,
            max_requests_per_window: 0,
            window_seconds: 0,
            granted_generation: 1,
        }]);
        let enforcer = BudgetEnforcer::new();
        enforcer.reconcile(&state).await;
        assert!(enforcer.try_admit(&state, &node).await.is_ok());
        // Reconcile with empty budgets.
        let state2 = make_state(vec![]);
        enforcer.reconcile(&state2).await;
        let result = enforcer.try_admit(&state2, &node).await;
        assert!(matches!(result, Err(BudgetError::NoBudget)));
    }
}
