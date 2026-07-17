use libp2p::{
    core::{transport::PortUse, upgrade::ReadyUpgrade, Endpoint, Multiaddr},
    swarm::{
        behaviour::FromSwarm, handler::ConnectionEvent, ConnectionDenied, ConnectionHandler,
        ConnectionHandlerEvent, ConnectionId, NetworkBehaviour, NotifyHandler, Stream,
        StreamProtocol, SubstreamProtocol, THandler, THandlerInEvent, THandlerOutEvent, ToSwarm,
    },
    PeerId,
};
use std::{
    collections::VecDeque,
    task::{Context, Poll},
};

pub const INFERENCE_PROTOCOL: &str = "/dllm/inference/1";

// ---------------------------------------------------------------------------
// Application-level types
// ---------------------------------------------------------------------------

/// An event from the stream handler/behaviour to the application.
#[derive(Debug)]
pub enum AppEvent {
    Inbound { peer: PeerId, stream: Stream },
    OutboundReady { stream: Stream, tag: u64 },
    OutboundError { tag: u64 },
}

/// A command from the application to open a stream to a peer.
#[derive(Debug, Clone)]
pub struct OpenStreamCmd {
    pub peer_id: PeerId,
    pub tag: u64,
}

// ---------------------------------------------------------------------------
// ConnectionHandler types (internal to the handler <-> behaviour protocol)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum HandlerEvent {
    Inbound { stream: Stream },
    Outbound { stream: Stream, tag: u64 },
    OutboundError { tag: u64 },
}

#[derive(Debug)]
pub enum HandlerCommand {
    OpenStream { tag: u64 },
}

// ---------------------------------------------------------------------------
// ConnectionHandler
// ---------------------------------------------------------------------------

pub struct Handler {
    listen_protocol: SubstreamProtocol<ReadyUpgrade<StreamProtocol>, ()>,
    pending_commands: VecDeque<HandlerCommand>,
    events_out: VecDeque<HandlerEvent>,
}

impl Handler {
    fn new() -> Self {
        Self {
            listen_protocol: SubstreamProtocol::new(
                ReadyUpgrade::new(StreamProtocol::new(INFERENCE_PROTOCOL)),
                (),
            ),
            pending_commands: VecDeque::new(),
            events_out: VecDeque::new(),
        }
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = HandlerCommand;
    type ToBehaviour = HandlerEvent;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = u64;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        self.listen_protocol.clone()
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        self.pending_commands.push_back(event);
    }

    fn connection_keep_alive(&self) -> bool {
        true
    }

    fn poll(
        &mut self,
        _cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        if let Some(event) = self.events_out.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }
        if let Some(cmd) = self.pending_commands.pop_front() {
            match cmd {
                HandlerCommand::OpenStream { tag } => {
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(
                            ReadyUpgrade::new(StreamProtocol::new(INFERENCE_PROTOCOL)),
                            tag,
                        ),
                    });
                }
            }
        }
        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(negotiated) => {
                self.events_out.push_back(HandlerEvent::Inbound {
                    stream: negotiated.protocol,
                });
            }
            ConnectionEvent::FullyNegotiatedOutbound(negotiated) => {
                self.events_out.push_back(HandlerEvent::Outbound {
                    stream: negotiated.protocol,
                    tag: negotiated.info,
                });
            }
            ConnectionEvent::DialUpgradeError(error) => {
                self.events_out
                    .push_back(HandlerEvent::OutboundError { tag: error.info });
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// NetworkBehaviour
// ---------------------------------------------------------------------------

pub struct Behaviour {
    commands_rx: tokio::sync::mpsc::UnboundedReceiver<OpenStreamCmd>,
    events_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    pending_commands: VecDeque<OpenStreamCmd>,
    connected_peers: std::collections::HashSet<PeerId>,
    queued_dials: VecDeque<OpenStreamCmd>,
}

impl Behaviour {
    pub fn new(
        commands_rx: tokio::sync::mpsc::UnboundedReceiver<OpenStreamCmd>,
        events_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            commands_rx,
            events_tx,
            pending_commands: VecDeque::new(),
            connected_peers: std::collections::HashSet::new(),
            queued_dials: VecDeque::new(),
        }
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = AppEvent;

    fn handle_established_inbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(Handler::new())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _connection_id: ConnectionId,
        _peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        Ok(Handler::new())
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(info) => {
                let peer = info.peer_id;
                self.connected_peers.insert(peer);
                let mut i = 0;
                while i < self.queued_dials.len() {
                    if self.queued_dials[i].peer_id == peer {
                        if let Some(cmd) = self.queued_dials.remove(i) {
                            self.pending_commands.push_back(cmd);
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            FromSwarm::ConnectionClosed(info) => {
                self.connected_peers.remove(&info.peer_id);
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        peer_id: PeerId,
        _connection_id: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {
            HandlerEvent::Inbound { stream } => {
                let _ = self.events_tx.send(AppEvent::Inbound {
                    peer: peer_id,
                    stream,
                });
            }
            HandlerEvent::Outbound { stream, tag } => {
                let _ = self.events_tx.send(AppEvent::OutboundReady { stream, tag });
            }
            HandlerEvent::OutboundError { tag } => {
                let _ = self.events_tx.send(AppEvent::OutboundError { tag });
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ToSwarm<Self::ToSwarm, THandlerInEvent<Self>>> {
        // Drain commands using poll_recv to register the waker.
        // Only process one command per poll; the swarm will re-poll if the waker fires.
        match self.commands_rx.poll_recv(cx) {
            Poll::Ready(Some(cmd)) => {
                if self.connected_peers.contains(&cmd.peer_id) {
                    return Poll::Ready(ToSwarm::NotifyHandler {
                        peer_id: cmd.peer_id,
                        handler: NotifyHandler::Any,
                        event: HandlerCommand::OpenStream { tag: cmd.tag },
                    });
                }
                let peer_id = cmd.peer_id;
                self.queued_dials.push_back(cmd);
                return Poll::Ready(ToSwarm::Dial {
                    opts: libp2p::swarm::dial_opts::DialOpts::from(peer_id),
                });
            }
            Poll::Ready(None) => return Poll::Pending,
            Poll::Pending => {}
        }

        if let Some(cmd) = self.pending_commands.pop_front() {
            return Poll::Ready(ToSwarm::NotifyHandler {
                peer_id: cmd.peer_id,
                handler: NotifyHandler::Any,
                event: HandlerCommand::OpenStream { tag: cmd.tag },
            });
        }

        Poll::Pending
    }
}
