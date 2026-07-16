use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SignedEndpointBinding {
    pub node_key: [u8; 32],
    pub endpoint: Vec<u8>,
    pub generation: u64,
    pub expires_at_ms: u64,
    pub signature: Signature,
}

impl SignedEndpointBinding {
    pub fn sign(
        node_key: [u8; 32],
        endpoint: Vec<u8>,
        generation: u64,
        expires_at_ms: u64,
        owner: &SigningKey,
    ) -> Self {
        let signature = owner.sign(&binding_bytes(
            node_key,
            &endpoint,
            generation,
            expires_at_ms,
        ));
        Self {
            node_key,
            endpoint,
            generation,
            expires_at_ms,
            signature,
        }
    }
}

#[derive(Debug)]
pub struct EndpointBindings {
    owner: VerifyingKey,
    current: HashMap<[u8; 32], SignedEndpointBinding>,
    revoked: HashSet<Vec<u8>>,
}

impl EndpointBindings {
    pub fn new(owner: VerifyingKey) -> Self {
        Self {
            owner,
            current: HashMap::new(),
            revoked: HashSet::new(),
        }
    }

    pub fn install(&mut self, binding: SignedEndpointBinding) -> Result<(), BindingError> {
        self.owner
            .verify(
                &binding_bytes(
                    binding.node_key,
                    &binding.endpoint,
                    binding.generation,
                    binding.expires_at_ms,
                ),
                &binding.signature,
            )
            .map_err(|_| BindingError::InvalidOwnerSignature)?;
        if self
            .current
            .get(&binding.node_key)
            .is_some_and(|current| current.generation >= binding.generation)
        {
            return Err(BindingError::StaleGeneration);
        }
        self.current.insert(binding.node_key, binding);
        Ok(())
    }

    pub fn revoke(&mut self, endpoint: &[u8]) {
        self.revoked.insert(endpoint.to_vec());
    }

    pub fn authorize(
        &self,
        node_key: [u8; 32],
        endpoint: &[u8],
        now_ms: u64,
    ) -> Result<(), BindingError> {
        if self.revoked.contains(endpoint) {
            return Err(BindingError::Revoked);
        }
        let binding = self
            .current
            .get(&node_key)
            .ok_or(BindingError::UnknownNode)?;
        if binding.endpoint != endpoint {
            return Err(BindingError::Rotated);
        }
        if binding.expires_at_ms < now_ms {
            return Err(BindingError::Expired);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingError {
    InvalidOwnerSignature,
    StaleGeneration,
    UnknownNode,
    Revoked,
    Rotated,
    Expired,
}

fn binding_bytes(
    node_key: [u8; 32],
    endpoint: &[u8],
    generation: u64,
    expires_at_ms: u64,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(48 + endpoint.len());
    bytes.extend_from_slice(b"dllm-endpoint-binding-v1");
    bytes.extend_from_slice(&node_key);
    bytes.extend_from_slice(&(endpoint.len() as u64).to_be_bytes());
    bytes.extend_from_slice(endpoint);
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&expires_at_ms.to_be_bytes());
    bytes
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferenceFrame {
    Start { request_id: u64, deadline_ms: u64 },
    Chunk { request_id: u64, bytes: Vec<u8> },
    Cancel { request_id: u64 },
    End { request_id: u64 },
}

#[derive(Debug)]
pub struct StreamBudget {
    max_concurrent: usize,
    active: HashMap<u64, u64>,
    cancelled: HashSet<u64>,
}

impl StreamBudget {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            active: HashMap::new(),
            cancelled: HashSet::new(),
        }
    }

    pub fn accept(&mut self, frame: &InferenceFrame, now_ms: u64) -> Result<(), StreamError> {
        match frame {
            InferenceFrame::Start {
                request_id,
                deadline_ms,
            } => {
                if *deadline_ms <= now_ms {
                    return Err(StreamError::DeadlineExceeded);
                }
                if self.active.len() >= self.max_concurrent {
                    return Err(StreamError::AtCapacity);
                }
                self.active.insert(*request_id, *deadline_ms);
            }
            InferenceFrame::Chunk { request_id, .. } => {
                if self.cancelled.contains(request_id) {
                    return Err(StreamError::Cancelled);
                }
                let deadline = self
                    .active
                    .get(request_id)
                    .ok_or(StreamError::UnknownRequest)?;
                if *deadline <= now_ms {
                    self.active.remove(request_id);
                    return Err(StreamError::DeadlineExceeded);
                }
            }
            InferenceFrame::Cancel { request_id } => {
                if self.active.remove(request_id).is_none() {
                    return Err(StreamError::UnknownRequest);
                }
                self.cancelled.insert(*request_id);
            }
            InferenceFrame::End { request_id } => {
                if self.active.remove(request_id).is_none() {
                    return Err(StreamError::UnknownRequest);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    AtCapacity,
    DeadlineExceeded,
    Cancelled,
    UnknownRequest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_signed_rotation_revocation_and_replay_are_enforced() {
        let owner = SigningKey::from_bytes(&[9; 32]);
        let node = [3; 32];
        let first = SignedEndpointBinding::sign(node, b"endpoint-a".to_vec(), 1, 10_000, &owner);
        let second = SignedEndpointBinding::sign(node, b"endpoint-b".to_vec(), 2, 20_000, &owner);
        let mut bindings = EndpointBindings::new(owner.verifying_key());

        bindings.install(first.clone()).unwrap();
        bindings.authorize(node, b"endpoint-a", 5_000).unwrap();
        bindings.install(second).unwrap();
        assert_eq!(
            bindings.authorize(node, b"endpoint-a", 5_000),
            Err(BindingError::Rotated)
        );
        assert_eq!(bindings.install(first), Err(BindingError::StaleGeneration));
        bindings.revoke(b"endpoint-b");
        assert_eq!(
            bindings.authorize(node, b"endpoint-b", 5_000),
            Err(BindingError::Revoked)
        );
    }

    #[test]
    fn stream_budget_enforces_concurrency_cancellation_and_deadlines() {
        let mut budget = StreamBudget::new(2);
        budget
            .accept(
                &InferenceFrame::Start {
                    request_id: 1,
                    deadline_ms: 100,
                },
                0,
            )
            .unwrap();
        budget
            .accept(
                &InferenceFrame::Start {
                    request_id: 2,
                    deadline_ms: 100,
                },
                0,
            )
            .unwrap();
        assert_eq!(
            budget.accept(
                &InferenceFrame::Start {
                    request_id: 3,
                    deadline_ms: 100,
                },
                0
            ),
            Err(StreamError::AtCapacity)
        );
        budget
            .accept(&InferenceFrame::Cancel { request_id: 1 }, 1)
            .unwrap();
        assert_eq!(
            budget.accept(
                &InferenceFrame::Chunk {
                    request_id: 1,
                    bytes: vec![1],
                },
                2
            ),
            Err(StreamError::Cancelled)
        );
        assert_eq!(
            budget.accept(
                &InferenceFrame::Chunk {
                    request_id: 2,
                    bytes: vec![2],
                },
                100
            ),
            Err(StreamError::DeadlineExceeded)
        );
    }
}
