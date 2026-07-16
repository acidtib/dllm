use iroh::{endpoint::Connection, EndpointId};
use std::collections::HashMap;
use thiserror::Error;

pub const DLLM_ALPN: &[u8] = b"dllm/peer/1";

#[derive(Debug, Clone, Default)]
pub struct PeerAuthorizer {
    peers: HashMap<EndpointId, [u8; 32]>,
}

impl PeerAuthorizer {
    pub fn insert(&mut self, endpoint_id: EndpointId, node_pubkey: [u8; 32]) {
        self.peers.insert(endpoint_id, node_pubkey);
    }

    pub fn authorize(&self, connection: &Connection) -> Result<[u8; 32], AuthorizationError> {
        self.peers
            .get(&connection.remote_id())
            .copied()
            .ok_or(AuthorizationError::UnknownEndpoint(connection.remote_id()))
    }
}

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error("unknown iroh endpoint {0}")]
    UnknownEndpoint(EndpointId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::{endpoint::presets, Endpoint};
    use std::time::Duration;

    async fn connect_pair() -> (Endpoint, Endpoint, Connection, Connection) {
        let server = Endpoint::builder(presets::Minimal)
            .alpns(vec![DLLM_ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let client = Endpoint::bind(presets::Minimal).await.unwrap();
        let server_addr = server.addr();
        let connect = tokio::spawn({
            let client = client.clone();
            async move { client.connect(server_addr, DLLM_ALPN).await.unwrap() }
        });
        let incoming = tokio::time::timeout(Duration::from_secs(5), server.accept())
            .await
            .unwrap()
            .unwrap();
        let server_connection = incoming.await.unwrap();
        let client_connection = connect.await.unwrap();
        (server, client, server_connection, client_connection)
    }

    #[tokio::test]
    async fn authenticated_endpoint_is_bound_to_dllm_identity() {
        let (server, client, server_connection, client_connection) = connect_pair().await;
        let node_pubkey = [7; 32];
        let mut authorizer = PeerAuthorizer::default();
        authorizer.insert(client.id(), node_pubkey);

        assert_eq!(
            authorizer.authorize(&server_connection).unwrap(),
            node_pubkey
        );

        let (mut send, mut receive) = client_connection.open_bi().await.unwrap();
        send.write_all(b"peer health").await.unwrap();
        send.finish().unwrap();
        let (mut response, mut request) = server_connection.accept_bi().await.unwrap();
        assert_eq!(request.read_to_end(1024).await.unwrap(), b"peer health");
        response.write_all(b"ready").await.unwrap();
        response.finish().unwrap();
        assert_eq!(receive.read_to_end(1024).await.unwrap(), b"ready");

        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn unknown_endpoint_is_rejected() {
        let (server, client, server_connection, _) = connect_pair().await;
        let error = PeerAuthorizer::default()
            .authorize(&server_connection)
            .unwrap_err();

        assert!(matches!(error, AuthorizationError::UnknownEndpoint(id) if id == client.id()));

        client.close().await;
        server.close().await;
    }
}
