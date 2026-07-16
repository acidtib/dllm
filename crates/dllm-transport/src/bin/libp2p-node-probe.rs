use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use libp2p::{
    autonat, dcutr, identify, identity, kad, noise, ping, relay, request_response,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, StreamProtocol,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, time::Duration};

const DLLM_PROTOCOL: &str = "/dllm/peer/1";
const FORWARDING_PROVIDER_KEY: &[u8] = b"/dllm/forwarding/v1";

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Id {
        seed: u8,
    },
    Forwarder {
        seed: u8,
        port: u16,
        bootstrap: Option<Multiaddr>,
    },
    Discover {
        seed: u8,
        port: u16,
        bootstrap: Multiaddr,
    },
    Listen {
        seed: u8,
        port: u16,
        relay: Multiaddr,
        allowed_peer: PeerId,
    },
    Dial {
        seed: u8,
        port: u16,
        relay: Multiaddr,
        target_peer: PeerId,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerRequest(String);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PeerResponse {
    accepted: bool,
    body: String,
}

#[derive(Clone)]
enum RelayAction {
    Listen(Multiaddr),
    Dial(Multiaddr, PeerId),
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
    autonat: autonat::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    ping: ping::Behaviour,
    request_response: request_response::cbor::Behaviour<PeerRequest, PeerResponse>,
}

#[derive(NetworkBehaviour)]
struct ForwardBehaviour {
    relay_server: relay::Behaviour,
    identify: identify::Behaviour,
    kademlia: kad::Behaviour<kad::store::MemoryStore>,
    ping: ping::Behaviour,
    request_response: request_response::cbor::Behaviour<PeerRequest, PeerResponse>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if let Command::Id { seed } = &args.command {
        println!("{}", key(*seed).public().to_peer_id());
        return Ok(());
    }
    if let Command::Forwarder {
        seed,
        port,
        bootstrap,
    } = &args.command
    {
        return run_forwarder(*seed, *port, bootstrap.clone()).await;
    }

    let (seed, port) = match &args.command {
        Command::Forwarder { seed, port, .. }
        | Command::Discover { seed, port, .. }
        | Command::Listen { seed, port, .. }
        | Command::Dial { seed, port, .. } => (*seed, *port),
        Command::Id { .. } => unreachable!(),
    };
    let local_key = key(seed);
    let local_peer = local_key.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_dns()?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|identity, relay_client| Behaviour {
            relay_client,
            dcutr: dcutr::Behaviour::new(local_peer),
            identify: identify::Behaviour::new(identify::Config::new(
                DLLM_PROTOCOL.into(),
                identity.public(),
            )),
            autonat: autonat::Behaviour::new(local_peer, Default::default()),
            kademlia: kad::Behaviour::new(local_peer, kad::store::MemoryStore::new(local_peer)),
            ping: ping::Behaviour::new(ping::Config::new()),
            request_response: request_response::cbor::Behaviour::new(
                [(
                    StreamProtocol::new(DLLM_PROTOCOL),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default(),
            ),
        })?
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    swarm
        .behaviour_mut()
        .kademlia
        .set_mode(Some(kad::Mode::Server));
    swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{port}").parse()?)?;
    swarm.listen_on(format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?)?;

    let mut discovering = false;
    let (allowed_peer, target, mut outbound_request, mut relay_action) = match args.command {
        Command::Forwarder { .. } => unreachable!(),
        Command::Discover { bootstrap, .. } => {
            discovering = true;
            (None, None, None, Some(RelayAction::Listen(bootstrap)))
        }
        Command::Listen {
            relay,
            allowed_peer: allowed,
            ..
        } => (Some(allowed), None, None, Some(RelayAction::Listen(relay))),
        Command::Dial {
            relay,
            target_peer,
            message,
            ..
        } => (
            None,
            Some(target_peer),
            Some(message),
            Some(RelayAction::Dial(relay, target_peer)),
        ),
        Command::Id { .. } => unreachable!(),
    };
    let relay = match relay_action.as_ref().expect("relay action is configured") {
        RelayAction::Listen(relay) | RelayAction::Dial(relay, _) => relay.clone(),
    };
    let relay_peer = relay.iter().find_map(|part| match part {
        libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
        _ => None,
    });
    if let Some(peer) = relay_peer {
        let mut routing_address = relay.clone();
        if matches!(
            routing_address.iter().last(),
            Some(libp2p::multiaddr::Protocol::P2p(_))
        ) {
            routing_address.pop();
        }
        swarm
            .behaviour_mut()
            .kademlia
            .add_address(&peer, routing_address);
    }
    let desired_relay_action = relay_action.clone();
    let mut circuit_listener = None;
    let mut retry_relay = false;
    let mut retry_timer = tokio::time::interval(Duration::from_secs(1));
    retry_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    swarm.dial(relay.clone())?;

    println!("node_ready peer_id={local_peer} forwarding_enabled=false");
    loop {
        let event = tokio::select! {
            _ = retry_timer.tick(), if retry_relay => {
                if !relay_peer.is_some_and(|peer| swarm.is_connected(&peer)) {
                    if let Err(error) = swarm.dial(relay.clone()) {
                        println!("relay_recovery=dial_deferred error={error}");
                    }
                }
                continue;
            }
            event = swarm.next() => match event {
                Some(event) => event,
                None => break,
            },
        };
        match event {
            SwarmEvent::NewListenAddr { address, .. } => println!("listen={address}"),
            SwarmEvent::ListenerClosed {
                listener_id,
                reason,
                ..
            } if circuit_listener == Some(listener_id) => {
                println!("relay_recovery=listener_closed reason={reason:?}");
                circuit_listener = None;
                relay_action = desired_relay_action.clone();
                retry_relay = true;
            }
            SwarmEvent::ConnectionClosed { peer_id, cause, .. }
                if relay_peer == Some(peer_id)
                    && matches!(desired_relay_action, Some(RelayAction::Listen(_))) =>
            {
                println!("relay_recovery=forwarder_disconnected cause={cause:?}");
                circuit_listener = None;
                relay_action = desired_relay_action.clone();
                retry_relay = true;
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                println!("connected peer_id={peer_id} endpoint={endpoint:?}");
                if target == Some(peer_id) {
                    if let Some(message) = outbound_request.take() {
                        swarm
                            .behaviour_mut()
                            .request_response
                            .send_request(&peer_id, PeerRequest(message));
                    }
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::RequestResponse(
                request_response::Event::Message { peer, message, .. },
            )) => match message {
                request_response::Message::Request {
                    request, channel, ..
                } => {
                    let accepted = allowed_peer == Some(peer);
                    let body = if accepted {
                        request.0
                    } else {
                        "endpoint is not an authorized DLLM member".into()
                    };
                    let _ = swarm
                        .behaviour_mut()
                        .request_response
                        .send_response(channel, PeerResponse { accepted, body });
                    println!("request peer_id={peer} accepted={accepted}");
                }
                request_response::Message::Response { response, .. } => {
                    println!(
                        "response accepted={} body={}",
                        response.accepted, response.body
                    );
                    return if response.accepted {
                        Ok(())
                    } else {
                        Err("remote rejected DLLM identity".into())
                    };
                }
            },
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
                println!("relay_client={event:?}");
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                println!("direct_upgrade={event:?}");
            }
            SwarmEvent::Behaviour(BehaviourEvent::Autonat(event)) => {
                println!("nat={event:?}");
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                println!(
                    "identified peer_id={peer_id} observed={}",
                    info.observed_addr
                );
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Sent {
                peer_id,
                ..
            })) => {
                if relay_peer == Some(peer_id) {
                    if discovering {
                        swarm
                            .behaviour_mut()
                            .kademlia
                            .get_providers(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY));
                        relay_action = None;
                        println!("discovery=query_started");
                        continue;
                    }
                    if let Some(action) = relay_action.take() {
                        match action {
                            RelayAction::Listen(relay) => {
                                circuit_listener = Some(swarm.listen_on(
                                    relay.with(libp2p::multiaddr::Protocol::P2pCircuit),
                                )?);
                                retry_relay = false;
                                println!("relay_recovery=reservation_requested");
                            }
                            RelayAction::Dial(relay, target_peer) => {
                                swarm.dial(
                                    relay
                                        .with(libp2p::multiaddr::Protocol::P2pCircuit)
                                        .with(libp2p::multiaddr::Protocol::P2p(target_peer)),
                                )?;
                            }
                        }
                    }
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
            )) if discovering => {
                let providers = providers
                    .into_iter()
                    .map(|peer| peer.to_string())
                    .collect::<Vec<_>>();
                println!("discovery=providers_found peers={}", providers.join(","));
                return if providers.is_empty() {
                    Err("no forwarding-capable node discovered".into())
                } else {
                    Ok(())
                };
            }
            _ => {}
        }
    }
    Ok(())
}

async fn run_forwarder(
    seed: u8,
    port: u16,
    bootstrap: Option<Multiaddr>,
) -> Result<(), Box<dyn Error>> {
    let local_key = key(seed);
    let local_peer = local_key.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_dns()?
        .with_behaviour(|identity| ForwardBehaviour {
            relay_server: relay::Behaviour::new(local_peer, relay::Config::default()),
            identify: identify::Behaviour::new(identify::Config::new(
                DLLM_PROTOCOL.into(),
                identity.public(),
            )),
            kademlia: kad::Behaviour::new(local_peer, kad::store::MemoryStore::new(local_peer)),
            ping: ping::Behaviour::new(ping::Config::new()),
            request_response: request_response::cbor::Behaviour::new(
                [(
                    StreamProtocol::new(DLLM_PROTOCOL),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default(),
            ),
        })?
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();
    swarm
        .behaviour_mut()
        .kademlia
        .set_mode(Some(kad::Mode::Server));
    swarm.listen_on(format!("/ip4/0.0.0.0/tcp/{port}").parse()?)?;
    swarm.listen_on(format!("/ip4/0.0.0.0/udp/{port}/quic-v1").parse()?)?;
    let bootstrap_peer = bootstrap.as_ref().and_then(|address| {
        address.iter().find_map(|part| match part {
            libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
            _ => None,
        })
    });
    if let (Some(peer), Some(address)) = (bootstrap_peer, bootstrap.as_ref()) {
        let mut routing_address = address.clone();
        if matches!(
            routing_address.iter().last(),
            Some(libp2p::multiaddr::Protocol::P2p(_))
        ) {
            routing_address.pop();
        }
        swarm
            .behaviour_mut()
            .kademlia
            .add_address(&peer, routing_address);
        swarm.dial(address.clone())?;
    }
    println!("node_ready peer_id={local_peer} forwarding_enabled=true");

    while let Some(event) = swarm.next().await {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => println!("listen={address}"),
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                println!("connected peer_id={peer_id}");
            }
            SwarmEvent::Behaviour(ForwardBehaviourEvent::RelayServer(event)) => {
                println!("forwarding={event:?}");
            }
            SwarmEvent::Behaviour(ForwardBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                swarm.add_external_address(info.observed_addr.clone());
                println!(
                    "identified peer_id={peer_id} observed={}",
                    info.observed_addr
                );
            }
            SwarmEvent::Behaviour(ForwardBehaviourEvent::Identify(identify::Event::Sent {
                peer_id,
                ..
            })) if bootstrap_peer == Some(peer_id) => {
                swarm
                    .behaviour_mut()
                    .kademlia
                    .start_providing(kad::RecordKey::new(&FORWARDING_PROVIDER_KEY))?;
                println!("discovery=forwarding_capability_published");
            }
            SwarmEvent::Behaviour(ForwardBehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::StartProviding(result),
                    ..
                },
            )) => println!("discovery=publication_result result={result:?}"),
            _ => {}
        }
    }
    Ok(())
}

fn key(seed: u8) -> identity::Keypair {
    let mut bytes = [0; 32];
    bytes[0] = seed;
    identity::Keypair::ed25519_from_bytes(bytes).expect("seed has the required length")
}
