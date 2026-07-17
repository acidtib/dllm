use futures_util::StreamExt;
use libp2p::{
    autonat, dcutr, identify, identity, kad, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};
pub use libp2p::{Multiaddr, PeerId};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

use crate::stream_handler::{self, AppEvent, OpenStreamCmd};

const IDENTIFY_PROTOCOL: &str = "/dllm/peer/1";
const FORWARDING_PROVIDER_KEY: &[u8] = b"/dllm/forwarding/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    Listed,
    Unlisted,
}

impl DiscoveryMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            DiscoveryMode::Listed => "listed",
            DiscoveryMode::Unlisted => "unlisted",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PeerNodeConfig {
    pub key_path: PathBuf,
    pub listen_port: u16,
    pub bootstrap: Vec<Multiaddr>,
    pub forwarding_enabled: bool,
    pub max_reservations: usize,
    pub eligible_forwarders: HashSet<PeerId>,
    pub reserve_forwarding_path: bool,
    pub discovery_mode: DiscoveryMode,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct PeerDiagnostics {
    pub enabled: bool,
    pub peer_id: Option<String>,
    pub forwarding_enabled: bool,
    pub bootstrap_peers: Vec<String>,
    pub discovered_providers: Vec<String>,
    pub selected_forwarder: Option<String>,
    pub reservation_active: bool,
    pub path: Option<String>,
    pub failed_connections: u64,
    pub reselections: u64,
    pub last_error: Option<String>,
    pub listen_addresses: Vec<String>,
    pub active_inbound_streams: u64,
    pub active_outbound_streams: u64,
    pub rejected_streams: u64,
    pub cancelled_streams: u64,
    pub deadline_expirations: u64,
    pub protocol_failures: u64,
    pub auth_failures: u64,
    pub last_app_error: Option<String>,
    pub last_stream_peer: Option<String>,
    pub last_stream_path: Option<String>,
    pub discovery_mode: String,
    pub published_discovery: bool,
}

#[derive(Clone)]
pub struct PeerNodeHandle {
    diagnostics: watch::Receiver<PeerDiagnostics>,
    task: Arc<JoinHandle<Result<(), PeerError>>>,
    stream_commands: mpsc::UnboundedSender<OpenStreamCmd>,
    stream_events: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AppEvent>>>,
    diagnostics_tx: watch::Sender<PeerDiagnostics>,
}

impl PeerNodeHandle {
    pub fn diagnostics(&self) -> watch::Receiver<PeerDiagnostics> {
        self.diagnostics.clone()
    }

    pub fn abort(&self) {
        self.task.abort();
    }

    pub fn open_stream(&self, cmd: OpenStreamCmd) {
        let _ = self.stream_commands.send(cmd);
    }

    pub async fn recv_stream_event(&self) -> Option<AppEvent> {
        self.stream_events.lock().await.recv().await
    }

    pub fn update_diagnostics(&self, f: impl FnOnce(&mut PeerDiagnostics)) {
        let mut d = self.diagnostics_tx.borrow().clone();
        f(&mut d);
        self.diagnostics_tx.send_replace(d);
    }
}

#[derive(Debug, Error)]
pub enum PeerError {
    #[error("peer identity storage error: {0}")]
    Storage(#[from] std::io::Error),
    #[error("peer identity encoding error: {0}")]
    Identity(#[from] identity::DecodingError),
    #[error("peer transport error: {0}")]
    Transport(String),
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    relay_server: relay::Behaviour,
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
    autonat: autonat::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    ping: ping::Behaviour,
    stream: stream_handler::Behaviour,
}

pub fn load_or_create_identity(path: &Path) -> Result<identity::Keypair, PeerError> {
    if path.exists() {
        return Ok(identity::Keypair::from_protobuf_encoding(&fs::read(path)?)?);
    }
    let key = identity::Keypair::generate_ed25519();
    let bytes = key
        .to_protobuf_encoding()
        .map_err(|error| PeerError::Transport(error.to_string()))?;
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(key)
}

pub fn start_peer_node(config: PeerNodeConfig) -> Result<PeerNodeHandle, PeerError> {
    if config.forwarding_enabled && config.discovery_mode == DiscoveryMode::Unlisted {
        return Err(PeerError::Transport(
            "unlisted nodes cannot be forwarding-eligible because edges discover \
             forwarders exclusively through the DHT"
                .into(),
        ));
    }
    if config.discovery_mode == DiscoveryMode::Unlisted && config.bootstrap.is_empty() {
        tracing::warn!(
            "unlisted node has no bootstrap peers and will not be discoverable by other nodes"
        );
    }
    let key = load_or_create_identity(&config.key_path)?;
    let peer_id = key.public().to_peer_id();
    let initial = PeerDiagnostics {
        enabled: true,
        peer_id: Some(peer_id.to_string()),
        forwarding_enabled: config.forwarding_enabled,
        bootstrap_peers: config
            .bootstrap
            .iter()
            .filter_map(peer_from_address)
            .map(|peer| peer.to_string())
            .collect(),
        discovery_mode: config.discovery_mode.as_str().into(),
        published_discovery: false,
        ..PeerDiagnostics::default()
    };
    let (status_tx, status_rx) = watch::channel(initial);
    let (stream_commands_tx, stream_commands_rx) = mpsc::unbounded_channel();
    let (stream_events_tx, stream_events_rx) = mpsc::unbounded_channel();
    let stream_behaviour = stream_handler::Behaviour::new(stream_commands_rx, stream_events_tx);
    let diag_tx = status_tx.clone();
    let task = tokio::spawn(run_peer_node(config, key, status_tx, stream_behaviour));
    Ok(PeerNodeHandle {
        diagnostics: status_rx,
        task: Arc::new(task),
        stream_commands: stream_commands_tx,
        stream_events: Arc::new(tokio::sync::Mutex::new(stream_events_rx)),
        diagnostics_tx: diag_tx,
    })
}

async fn run_peer_node(
    config: PeerNodeConfig,
    key: identity::Keypair,
    status_tx: watch::Sender<PeerDiagnostics>,
    stream_behaviour: stream_handler::Behaviour,
) -> Result<(), PeerError> {
    let local_peer = key.public().to_peer_id();
    let forwarding_enabled = config.forwarding_enabled;
    let max_reservations = config.max_reservations;
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(transport_error)?
        .with_quic()
        .with_dns()
        .map_err(transport_error)?
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(transport_error)?
        .with_behaviour(move |identity, relay_client| {
            let mut relay_config = relay::Config::default();
            relay_config.max_reservations = if forwarding_enabled {
                max_reservations
            } else {
                0
            };
            relay_config.max_circuits = relay_config.max_reservations;
            Behaviour {
                relay_client,
                relay_server: relay::Behaviour::new(local_peer, relay_config),
                dcutr: dcutr::Behaviour::new(local_peer),
                identify: identify::Behaviour::new(identify::Config::new(
                    IDENTIFY_PROTOCOL.into(),
                    identity.public(),
                )),
                autonat: autonat::Behaviour::new(local_peer, Default::default()),
                kademlia: kad::Behaviour::new(local_peer, kad::store::MemoryStore::new(local_peer)),
                ping: ping::Behaviour::new(ping::Config::new()),
                stream: stream_behaviour,
            }
        })
        .map_err(transport_error)?
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    swarm
        .behaviour_mut()
        .kademlia
        .set_mode(Some(kad::Mode::Server));
    swarm
        .listen_on(
            format!("/ip4/0.0.0.0/tcp/{}", config.listen_port)
                .parse()
                .map_err(transport_error)?,
        )
        .map_err(transport_error)?;
    swarm
        .listen_on(
            format!("/ip4/0.0.0.0/udp/{}/quic-v1", config.listen_port)
                .parse()
                .map_err(transport_error)?,
        )
        .map_err(transport_error)?;

    let bootstrap_addresses = config
        .bootstrap
        .iter()
        .filter_map(|address| peer_from_address(address).map(|peer| (peer, address.clone())))
        .collect::<HashMap<_, _>>();
    for (peer, address) in &bootstrap_addresses {
        let mut routing = address.clone();
        if matches!(
            routing.iter().last(),
            Some(libp2p::multiaddr::Protocol::P2p(_))
        ) {
            routing.pop();
        }
        swarm.behaviour_mut().kademlia.add_address(peer, routing);
        if let Err(error) = swarm.dial(address.clone()) {
            update_status(&status_tx, |status| {
                status.failed_connections += 1;
                status.last_error = Some(error.to_string());
            });
        }
    }

    let mut published_forwarding = false;
    let mut _published_node = false;
    let mut discovery_started = false;
    let mut selected = None;
    let mut selected_address = None;
    let mut connected_addresses = HashMap::new();
    let mut circuit_listener = None;
    let mut failed = HashSet::new();
    let mut retry = tokio::time::interval(Duration::from_secs(1));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let event = tokio::select! {
            _ = retry.tick(), if config.reserve_forwarding_path && selected.is_none() && discovery_started => {
                swarm.behaviour_mut().kademlia.get_providers(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY));
                continue;
            }
            event = swarm.next() => match event {
                Some(event) => event,
                None => return Ok(()),
            }
        };
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                if config.forwarding_enabled {
                    swarm.add_external_address(address.clone());
                }
                update_status(&status_tx, |status| {
                    let address = address.to_string();
                    if !status.listen_addresses.contains(&address) {
                        status.listen_addresses.push(address);
                        status.listen_addresses.sort();
                    }
                });
            }
            SwarmEvent::ConnectionClosed { peer_id, .. }
                if selected == Some(peer_id) && !swarm.is_connected(&peer_id) =>
            {
                selected = None;
                selected_address = None;
                circuit_listener = None;
                failed.insert(peer_id);
                update_status(&status_tx, |status| {
                    status.selected_forwarder = None;
                    status.reservation_active = false;
                    status.path = None;
                    status.reselections += 1;
                });
                swarm
                    .behaviour_mut()
                    .kademlia
                    .get_providers(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY));
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } if selected == Some(peer_id) && !swarm.is_connected(&peer_id) => {
                selected = None;
                selected_address = None;
                failed.insert(peer_id);
                update_status(&status_tx, |status| {
                    status.failed_connections += 1;
                    status.selected_forwarder = None;
                    status.last_error = Some(error.to_string());
                    status.reselections += 1;
                });
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                let mut address = endpoint.get_remote_address().clone();
                if !matches!(
                    address.iter().last(),
                    Some(libp2p::multiaddr::Protocol::P2p(_))
                ) {
                    address.push(libp2p::multiaddr::Protocol::P2p(peer_id));
                }
                connected_addresses.insert(peer_id, address.clone());
                if selected == Some(peer_id) {
                    selected_address = Some(address);
                }
            }
            SwarmEvent::ListenerClosed { listener_id, .. }
                if circuit_listener == Some(listener_id) =>
            {
                circuit_listener = None;
                update_status(&status_tx, |status| {
                    status.reservation_active = false;
                    status.path = None;
                });
                if let Some(address) = selected_address.clone() {
                    circuit_listener = Some(
                        swarm
                            .listen_on(address.with(libp2p::multiaddr::Protocol::P2pCircuit))
                            .map_err(transport_error)?,
                    );
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                if config.forwarding_enabled {
                    swarm.add_external_address(info.observed_addr);
                    if !published_forwarding && config.discovery_mode == DiscoveryMode::Listed {
                        swarm
                            .behaviour_mut()
                            .kademlia
                            .start_providing(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY))
                            .map_err(transport_error)?;
                        published_forwarding = true;
                        update_status(&status_tx, |status| {
                            status.published_discovery = true;
                        });
                    }
                }
                if bootstrap_addresses.contains_key(&peer_id)
                    && config.reserve_forwarding_path
                    && !discovery_started
                {
                    discovery_started = true;
                    swarm
                        .behaviour_mut()
                        .kademlia
                        .get_providers(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY));
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Sent {
                peer_id,
                ..
            })) if selected == Some(peer_id) && circuit_listener.is_none() => {
                if let Some(address) = selected_address.clone() {
                    circuit_listener = Some(
                        swarm
                            .listen_on(address.with(libp2p::multiaddr::Protocol::P2pCircuit))
                            .map_err(transport_error)?,
                    );
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    result:
                        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                            providers,
                            ..
                        })),
                    ..
                },
            )) if config.reserve_forwarding_path && selected.is_none() => {
                let mut providers = providers.into_iter().collect::<Vec<_>>();
                providers.sort_by_key(|peer| peer.to_bytes());
                update_status(&status_tx, |status| {
                    status.discovered_providers =
                        providers.iter().map(ToString::to_string).collect();
                });
                providers.retain(|peer| config.eligible_forwarders.contains(peer));
                let eligible = providers.clone();
                providers.retain(|peer| !failed.contains(peer));
                if providers.is_empty() && !eligible.is_empty() {
                    failed.clear();
                    providers = eligible;
                }
                if let Some(provider) = providers.into_iter().next() {
                    selected = Some(provider);
                    update_status(&status_tx, |status| {
                        status.selected_forwarder = Some(provider.to_string());
                        status.last_error = None;
                    });
                    if let Some(address) = connected_addresses.get(&provider).cloned() {
                        selected_address = Some(address.clone());
                        circuit_listener = Some(
                            swarm
                                .listen_on(address.with(libp2p::multiaddr::Protocol::P2pCircuit))
                                .map_err(transport_error)?,
                        );
                    } else if let Err(error) = swarm.dial(provider) {
                        selected = None;
                        update_status(&status_tx, |status| {
                            status.failed_connections += 1;
                            status.last_error = Some(error.to_string());
                        });
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) if selected == Some(relay_peer_id) => {
                update_status(&status_tx, |status| {
                    status.reservation_active = true;
                    status.path = Some("forwarded".into());
                });
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) if event.result.is_ok() => {
                update_status(&status_tx, |status| {
                    status.path = Some("direct".into());
                });
            }
            _ => {}
        }
    }
}

fn peer_from_address(address: &Multiaddr) -> Option<PeerId> {
    address.iter().find_map(|protocol| match protocol {
        libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn update_status(
    sender: &watch::Sender<PeerDiagnostics>,
    update: impl FnOnce(&mut PeerDiagnostics),
) {
    let mut status = sender.borrow().clone();
    update(&mut status);
    sender.send_replace(status);
}

fn transport_error(error: impl std::fmt::Display) -> PeerError {
    PeerError::Transport(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ordinary_nodes_discover_and_reserve_an_eligible_forwarder() {
        let directory =
            std::env::temp_dir().join(format!("dllm-peer-discovery-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let bootstrap_key_path = directory.join("bootstrap.key");
        let forwarder_key_path = directory.join("forwarder.key");
        let unapproved_key_path = directory.join("unapproved.key");
        let edge_key_path = directory.join("edge.key");
        let bootstrap_peer = load_or_create_identity(&bootstrap_key_path)
            .unwrap()
            .public()
            .to_peer_id();
        let forwarder_peer = load_or_create_identity(&forwarder_key_path)
            .unwrap()
            .public()
            .to_peer_id();
        load_or_create_identity(&unapproved_key_path).unwrap();
        let bootstrap_port = unused_port();
        let forwarder_port = unused_port();
        let edge_port = unused_port();
        let unapproved_port = unused_port();
        let bootstrap_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/{bootstrap_port}/p2p/{bootstrap_peer}")
                .parse()
                .unwrap();

        let bootstrap = start_peer_node(PeerNodeConfig {
            key_path: bootstrap_key_path,
            listen_port: bootstrap_port,
            bootstrap: Vec::new(),
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let forwarder = start_peer_node(PeerNodeConfig {
            key_path: forwarder_key_path,
            listen_port: forwarder_port,
            bootstrap: vec![bootstrap_address.clone()],
            forwarding_enabled: true,
            max_reservations: 4,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        let unapproved = start_peer_node(PeerNodeConfig {
            key_path: unapproved_key_path,
            listen_port: unapproved_port,
            bootstrap: vec![bootstrap_address.clone()],
            forwarding_enabled: true,
            max_reservations: 4,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let edge = start_peer_node(PeerNodeConfig {
            key_path: edge_key_path,
            listen_port: edge_port,
            bootstrap: vec![bootstrap_address],
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::from([forwarder_peer]),
            reserve_forwarding_path: true,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        let diagnostics = edge.diagnostics();

        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let status = diagnostics.borrow().clone();
                if status.reservation_active {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(result.selected_forwarder, Some(forwarder_peer.to_string()));
        assert_eq!(result.path.as_deref(), Some("forwarded"));
        assert_eq!(result.discovered_providers.len(), 2);

        edge.abort();
        forwarder.abort();
        unapproved.abort();
        bootstrap.abort();
        for path in [
            directory.join("bootstrap.key"),
            directory.join("forwarder.key"),
            directory.join("unapproved.key"),
            directory.join("edge.key"),
        ] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn forwarding_loss_reselects_another_eligible_node() {
        let directory =
            std::env::temp_dir().join(format!("dllm-peer-recovery-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let paths = [
            directory.join("bootstrap.key"),
            directory.join("forwarder-a.key"),
            directory.join("forwarder-b.key"),
            directory.join("edge.key"),
        ];
        let peers = paths
            .iter()
            .map(|path| load_or_create_identity(path).unwrap().public().to_peer_id())
            .collect::<Vec<_>>();
        let ports = (0..4).map(|_| unused_port()).collect::<Vec<_>>();
        let bootstrap_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/{}/p2p/{}", ports[0], peers[0])
                .parse()
                .unwrap();
        let bootstrap = start_peer_node(PeerNodeConfig {
            key_path: paths[0].clone(),
            listen_port: ports[0],
            bootstrap: Vec::new(),
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let forwarder_a = start_peer_node(PeerNodeConfig {
            key_path: paths[1].clone(),
            listen_port: ports[1],
            bootstrap: vec![bootstrap_address.clone()],
            forwarding_enabled: true,
            max_reservations: 4,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        let forwarder_b = start_peer_node(PeerNodeConfig {
            key_path: paths[2].clone(),
            listen_port: ports[2],
            bootstrap: vec![bootstrap_address.clone()],
            forwarding_enabled: true,
            max_reservations: 4,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let edge = start_peer_node(PeerNodeConfig {
            key_path: paths[3].clone(),
            listen_port: ports[3],
            bootstrap: vec![bootstrap_address],
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::from([peers[1], peers[2]]),
            reserve_forwarding_path: true,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        let diagnostics = edge.diagnostics();
        let first = wait_for_status(&diagnostics, Duration::from_secs(15), |status| {
            status.reservation_active
        })
        .await;
        let first_peer: PeerId = first.selected_forwarder.unwrap().parse().unwrap();
        if first_peer == peers[1] {
            forwarder_a.abort();
        } else {
            forwarder_b.abort();
        }
        let replacement = if first_peer == peers[1] {
            peers[2]
        } else {
            peers[1]
        };
        let replacement_string = replacement.to_string();
        let recovered = wait_for_status(&diagnostics, Duration::from_secs(15), |status| {
            status.reservation_active
                && status.selected_forwarder.as_deref() == Some(&replacement_string)
        })
        .await;
        assert!(recovered.reselections >= 1);

        edge.abort();
        forwarder_a.abort();
        forwarder_b.abort();
        bootstrap.abort();
        for path in paths {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    async fn wait_for_status(
        diagnostics: &watch::Receiver<PeerDiagnostics>,
        timeout: Duration,
        predicate: impl Fn(&PeerDiagnostics) -> bool,
    ) -> PeerDiagnostics {
        tokio::time::timeout(timeout, async {
            loop {
                let status = diagnostics.borrow().clone();
                if predicate(&status) {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "peer status condition timed out: {:?}",
                diagnostics.borrow()
            )
        })
    }

    fn unused_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlisted_nodes_do_not_publish() {
        let directory = std::env::temp_dir().join(format!(
            "dllm-unlisted-discover-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let bootstrap_key_path = directory.join("bootstrap.key");
        let unlisted_key_path = directory.join("unlisted.key");
        let bootstrap_peer = load_or_create_identity(&bootstrap_key_path)
            .unwrap()
            .public()
            .to_peer_id();
        let unlisted_peer = load_or_create_identity(&unlisted_key_path)
            .unwrap()
            .public()
            .to_peer_id();
        let bootstrap_port = unused_port();
        let unlisted_port = unused_port();
        let bootstrap_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/{bootstrap_port}/p2p/{bootstrap_peer}")
                .parse()
                .unwrap();

        let bootstrap = start_peer_node(PeerNodeConfig {
            key_path: bootstrap_key_path,
            listen_port: bootstrap_port,
            bootstrap: Vec::new(),
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Listed,
        })
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Unlisted node: not a forwarder, bootstrap is the bootstrap node.
        let unlisted = start_peer_node(PeerNodeConfig {
            key_path: unlisted_key_path,
            listen_port: unlisted_port,
            bootstrap: vec![bootstrap_address],
            forwarding_enabled: false,
            max_reservations: 0,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Unlisted,
        })
        .unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let diag = unlisted.diagnostics();
        let status = diag.borrow().clone();
        assert_eq!(
            status.discovery_mode, "unlisted",
            "diagnostics should report unlisted mode"
        );
        assert!(
            !status.published_discovery,
            "unlisted node should not set published_discovery"
        );

        // Query DHT from the bootstrap node; verify the unlisted node is not
        // among the forwarding providers.
        let bootstrap_diag = bootstrap.diagnostics();
        let bstatus = bootstrap_diag.borrow().clone();
        assert!(
            !bstatus
                .discovered_providers
                .contains(&unlisted_peer.to_string()),
            "unlisted node should not appear as a forwarding provider"
        );

        unlisted.abort();
        bootstrap.abort();
        for path in [
            directory.join("bootstrap.key"),
            directory.join("unlisted.key"),
        ] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn unlisted_forwarder_rejected_at_startup() {
        let directory = std::env::temp_dir().join(format!(
            "dllm-unlisted-forwarder-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let key_path = directory.join("node.key");
        let result = start_peer_node(PeerNodeConfig {
            key_path: key_path.clone(),
            listen_port: unused_port(),
            bootstrap: Vec::new(),
            forwarding_enabled: true,
            max_reservations: 4,
            eligible_forwarders: HashSet::new(),
            reserve_forwarding_path: false,
            discovery_mode: DiscoveryMode::Unlisted,
        });
        assert!(
            result.is_err(),
            "unlisted + forwarding_enabled should fail at startup"
        );
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("unlisted"),
            "error message should mention unlisted: {err}"
        );
        let _ = fs::remove_file(key_path);
        fs::remove_dir(directory).unwrap();
    }
}
