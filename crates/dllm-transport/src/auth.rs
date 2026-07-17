use dllm_protocol::NetworkState;
use libp2p::PeerId;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAuth {
    pub node_pubkey: [u8; 32],
    pub member: bool,
    pub owner: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AuthError {
    #[error("unknown transport peer ID")]
    UnknownPeer,
    #[error("transport binding has expired")]
    Expired,
    #[error("transport binding has been rotated")]
    Rotated,
    #[error("transport binding has been revoked")]
    Revoked,
    #[error("peer is not a current network member")]
    NotMember,
    #[error("signed state is absent or invalid")]
    StaleState,
    #[error("transport binding is superseded by a newer generation")]
    StaleBinding,
}

#[derive(Debug, Clone)]
pub struct AuthView {
    state_rx: watch::Receiver<Arc<NetworkState>>,
}

impl AuthView {
    pub fn new(state_rx: watch::Receiver<Arc<NetworkState>>) -> Self {
        Self { state_rx }
    }

    pub fn snapshot(&self) -> Arc<NetworkState> {
        self.state_rx.borrow().clone()
    }

    pub fn authorize(&self, peer_id: &PeerId, now_unix: u64) -> Result<PeerAuth, AuthError> {
        let state = self.state_rx.borrow();
        let peer_str = peer_id.to_string();

        let binding = state
            .transport_bindings
            .iter()
            .find(|binding| binding.transport_peer_id == peer_str)
            .ok_or(AuthError::UnknownPeer)?;

        if now_unix >= binding.expires_at_unix {
            return Err(AuthError::Expired);
        }

        if state.transport_revocations.iter().any(|revocation| {
            revocation.transport_peer_id == peer_str
                || (revocation.node_pubkey == binding.node_pubkey
                    && revocation.binding_generation >= binding.binding_generation)
        }) {
            return Err(AuthError::Revoked);
        }

        let multiple_bindings = state
            .transport_bindings
            .iter()
            .filter(|candidate| candidate.node_pubkey == binding.node_pubkey)
            .count();
        if multiple_bindings > 1 {
            return Err(AuthError::StaleState);
        }

        let owner = state.owner_pubkey == binding.node_pubkey;
        let member = owner
            || state
                .members
                .iter()
                .any(|member| member.node_pubkey == binding.node_pubkey);

        if !member {
            return Err(AuthError::NotMember);
        }

        Ok(PeerAuth {
            node_pubkey: binding.node_pubkey,
            member,
            owner,
        })
    }

    pub fn resolve_peer(&self, node_pubkey: &[u8; 32]) -> Option<PeerId> {
        let state = self.state_rx.borrow();
        state
            .transport_bindings
            .iter()
            .find(|binding| binding.node_pubkey == *node_pubkey)
            .and_then(|binding| binding.transport_peer_id.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dllm_protocol::{
        Member, NetworkState, TransportEndpointBinding, TransportEndpointRevocation, SCHEMA_VERSION,
    };
    use ed25519_dalek::SigningKey;

    const PEER_A: &str = "12D3KooWSahP5pFRCEfaziPEba7urXGeif6T1y8jmodzdFUvzBHj";
    const PEER_B: &str = "12D3KooWR2KSRQWyanR1dPvnZkXt296xgf3FFn8135szya3zYYwY";

    fn owner_key() -> SigningKey {
        SigningKey::from_bytes(&[1; 32])
    }

    fn make_state(
        bindings: Vec<TransportEndpointBinding>,
        revocations: Vec<TransportEndpointRevocation>,
        members: Vec<Member>,
    ) -> NetworkState {
        NetworkState {
            schema_version: SCHEMA_VERSION,
            network_id: uuid::Uuid::from_bytes([0; 16]),
            name: "test".into(),
            owner_pubkey: owner_key().verifying_key().to_bytes(),
            generation: 1,
            members,
            model_assignments: vec![],
            placements: vec![],
            hardware_profiles: vec![],
            transport_bindings: bindings,
            transport_revocations: revocations,
            forwarding_policy: vec![],
        }
    }

    fn member() -> Member {
        Member {
            node_pubkey: [2; 32],
            endpoint: "http://127.0.0.1:7337".into(),
            relay_endpoint: None,
            joined_generation: 1,
        }
    }

    #[test]
    fn valid_binding_accepted() {
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding.clone()], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        let result = view.authorize(&peer, 150).unwrap();
        assert_eq!(result.node_pubkey, [2; 32]);
        assert!(result.member);
    }

    #[test]
    fn unknown_peer_rejected() {
        let state = make_state(vec![], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::UnknownPeer)
        ));
    }

    #[test]
    fn non_member_binding_rejected() {
        let binding = TransportEndpointBinding {
            node_pubkey: [3; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::NotMember)
        ));
    }

    #[test]
    fn expired_binding_rejected() {
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 200),
            Err(AuthError::Expired)
        ));
    }

    #[test]
    fn rotated_binding_rejected() {
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_B.into(),
            binding_generation: 2,
            issued_at_unix: 100,
            expires_at_unix: 300,
        };
        let state = make_state(vec![binding], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        // PEER_A is not in bindings but PEER_B is (rotation away from A)
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::UnknownPeer)
        ));
    }

    #[test]
    fn revocation_rejects_new_request() {
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 300,
        };
        let revocation = TransportEndpointRevocation {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            revoked_at_unix: 150,
        };
        let state = make_state(vec![binding], vec![revocation], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 175),
            Err(AuthError::Revoked)
        ));
    }

    #[test]
    fn live_state_update_changes_authz_outcome() {
        let binding_a = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 300,
        };
        let state1 = make_state(vec![binding_a.clone()], vec![], vec![member()]);
        let (tx, rx) = watch::channel(Arc::new(state1));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(view.authorize(&peer, 150).is_ok());

        // Update state with revocation
        let revocation = TransportEndpointRevocation {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            revoked_at_unix: 175,
        };
        let state2 = make_state(vec![binding_a], vec![revocation], vec![member()]);
        tx.send_replace(Arc::new(state2));

        assert!(matches!(
            view.authorize(&peer, 200),
            Err(AuthError::Revoked)
        ));
    }

    #[test]
    fn stale_or_invalid_state_fails_closed() {
        let (_tx, rx) = watch::channel(Arc::new(make_state(vec![], vec![], vec![])));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        // No bindings at all
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::UnknownPeer)
        ));
    }

    #[test]
    fn resolve_peer_maps_node_to_peer_id() {
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        assert_eq!(view.resolve_peer(&[2; 32]), Some(PEER_A.parse().unwrap()));
        assert_eq!(view.resolve_peer(&[9; 32]), None);
    }

    #[test]
    fn owner_is_authorized_as_member() {
        let owner = owner_key().verifying_key().to_bytes();
        let binding = TransportEndpointBinding {
            node_pubkey: owner,
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        let result = view.authorize(&peer, 150).unwrap();
        assert_eq!(result.node_pubkey, owner);
        assert!(result.member);
        assert!(result.owner);
    }

    #[test]
    fn tombstoned_binding_rejected() {
        // A binding that exists alongside a revocation tombstone for the same peer
        // and generation is rejected. This models explicit endpoint revocation.
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 300,
        };
        let revocation = TransportEndpointRevocation {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            revoked_at_unix: 120,
        };
        let state = make_state(vec![binding], vec![revocation], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 140),
            Err(AuthError::Revoked)
        ));
    }

    #[test]
    fn tombstone_with_higher_generation_rejects_stale_rebinding() {
        // If a binding was revoked at generation 1 and a new binding at gen 2
        // references a different peer ID (PEER_B), the old peer (PEER_A) stays
        // rejected because the revocation records node_pubkey + gen >= 1.
        let binding_b = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_B.into(),
            binding_generation: 2,
            issued_at_unix: 100,
            expires_at_unix: 300,
        };
        let revocation_a = TransportEndpointRevocation {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            revoked_at_unix: 99,
        };
        let state = make_state(vec![binding_b], vec![revocation_a], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer_a: PeerId = PEER_A.parse().unwrap();
        let peer_b: PeerId = PEER_B.parse().unwrap();

        // PEER_A is unknown (binding removed, tombstone present).
        assert!(matches!(
            view.authorize(&peer_a, 150),
            Err(AuthError::UnknownPeer)
        ));
        // PEER_B is the active rotated binding.
        let result = view.authorize(&peer_b, 150).unwrap();
        assert_eq!(result.node_pubkey, [2; 32]);
        assert!(result.member);
    }

    #[test]
    fn member_revocation_tombstones_transport_binding() {
        // When a member is revoked from the network, their transport binding must
        // also be tombstoned in the same state update. Verify the auth view
        // rejects the tombstoned peer.
        let node = [2; 32];
        // The binding was removed from transport_bindings by revoke_member;
        // only the tombstone remains.
        let revocation = TransportEndpointRevocation {
            node_pubkey: node,
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            revoked_at_unix: 150,
        };
        // Member is no longer in the member list, binding is tombstoned.
        let state = make_state(vec![], vec![revocation], vec![]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();

        // Without a binding in transport_bindings, UnknownPeer fires before
        // Revoked. This is correct: the binding was removed, the tombstone
        // prevents rebinding at the same generation.
        assert!(matches!(
            view.authorize(&peer, 160),
            Err(AuthError::UnknownPeer)
        ));
    }

    #[test]
    fn signed_state_replica_works_without_owner_key() {
        // An AuthView backed by verified signed state works on member nodes
        // that do not hold the owner private key.
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        // AuthView never accesses the owner key — it only uses the signed state.
        let result = view.authorize(&peer, 150).unwrap();
        assert_eq!(result.node_pubkey, [2; 32]);
        assert!(result.member);
        assert!(!result.owner);
    }

    #[test]
    fn stale_generation_state_is_still_readable() {
        // A state with generation 1 is loaded. A newer generation (2) exists
        // elsewhere. The view still authorizes from the loaded state — it does
        // not fail just because the generation is not the latest. It is the
        // caller's responsibility to update the view.
        let binding = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(view.authorize(&peer, 150).is_ok());
    }

    #[test]
    fn duplicate_node_bindings_detected() {
        // If state somehow has two active bindings for the same node, the auth
        // view must detect this as StaleState and fail closed.
        let binding1 = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let binding2 = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_B.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let state = make_state(vec![binding1, binding2], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::StaleState)
        ));
    }

    #[test]
    fn connection_peerid_is_authoritative_not_frame_identity() {
        // The AuthView always derives identity from the connection's PeerId
        // (Noise-authenticated by libp2p). A caller cannot pass a different
        // PeerId in an application frame to impersonate another node.
        //
        // This test verifies that authorizing PEER_A returns the node bound
        // to PEER_A, not the node bound to PEER_B, even if both exist.
        let binding_a = TransportEndpointBinding {
            node_pubkey: [2; 32],
            transport_peer_id: PEER_A.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let binding_b = TransportEndpointBinding {
            node_pubkey: [3; 32],
            transport_peer_id: PEER_B.into(),
            binding_generation: 1,
            issued_at_unix: 100,
            expires_at_unix: 200,
        };
        let member_b = Member {
            node_pubkey: [3; 32],
            endpoint: "http://127.0.0.1:7338".into(),
            relay_endpoint: None,
            joined_generation: 1,
        };
        let state = make_state(
            vec![binding_a.clone(), binding_b],
            vec![],
            vec![member(), member_b],
        );
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);

        // Peer A resolves to node [2; 32], not [3; 32].
        let peer_a: PeerId = PEER_A.parse().unwrap();
        let result = view.authorize(&peer_a, 150).unwrap();
        assert_eq!(result.node_pubkey, [2; 32]);

        // Peer B resolves to node [3; 32], not [2; 32].
        let peer_b: PeerId = PEER_B.parse().unwrap();
        let result = view.authorize(&peer_b, 150).unwrap();
        assert_eq!(result.node_pubkey, [3; 32]);
    }

    #[test]
    fn empty_bindings_with_members_still_requires_binding() {
        // Having members in the network state does not grant auth. A peer
        // must have a valid, active transport binding.
        let state = make_state(vec![], vec![], vec![member()]);
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);
        let peer: PeerId = PEER_A.parse().unwrap();
        assert!(matches!(
            view.authorize(&peer, 150),
            Err(AuthError::UnknownPeer)
        ));
    }

    /// Authorization is structurally independent of discovery mode:
    /// `AuthView::authorize` does not take a discovery mode parameter,
    /// so the same authorization checks apply regardless of how the
    /// caller was discovered. This test confirms a valid binding passes
    /// and an unknown peer is rejected.
    #[test]
    fn both_discovery_modes_enforce_authorization() {
        let owner = owner_key();
        let state = make_state(
            vec![TransportEndpointBinding {
                node_pubkey: owner.verifying_key().to_bytes(),
                transport_peer_id: PEER_A.into(),
                binding_generation: 1,
                issued_at_unix: 100,
                expires_at_unix: 200,
            }],
            vec![],
            vec![Member {
                node_pubkey: owner.verifying_key().to_bytes(),
                endpoint: "http://example.com".into(),
                relay_endpoint: None,
                joined_generation: 1,
            }],
        );
        let (_tx, rx) = watch::channel(Arc::new(state));
        let view = AuthView::new(rx);

        // Valid binding passes.
        let known: PeerId = PEER_A.parse().unwrap();
        assert!(view.authorize(&known, 150).is_ok());

        // Unknown peer is rejected regardless of how it was discovered.
        let unknown: PeerId = PEER_B.parse().unwrap();
        assert!(matches!(
            view.authorize(&unknown, 150),
            Err(AuthError::UnknownPeer)
        ));
    }
}
