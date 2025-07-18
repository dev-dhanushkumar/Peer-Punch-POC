use anyhow::Result;
use peer_punch::Message;
use proxy_header::{ProxyHeader, ParseConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

#[derive(Debug, Clone, PartialEq)]
enum PeerStatus {
    Idle,
    Busy(String), // Now stores the username of the peer it's busy with
}

#[derive(Debug, Clone)]
struct PeerInfo {
    public_addr: SocketAddr, // The address reported by the peer (from STUN)
    source_addr: SocketAddr, // The address the server sees
    status: PeerStatus,
}

struct RelayServer {
    socket: UdpSocket,
    peers: HashMap<String, PeerInfo>, // Keyed by username
}

impl RelayServer {
    async fn new(addr: &str) -> Result<Self> {
        let socket = UdpSocket::bind(addr).await?;
        log::info!("Relay server listening on {}", addr);
        Ok(Self {
            socket,
            peers: HashMap::new(),
        })
    }

    async fn run(&mut self) -> Result<()> {
        let mut buf = [0; 1024];
        loop {
            let (len, mut source_addr) = self.socket.recv_from(&mut buf).await?;
            let mut data = &buf[..len];

            // Attempt to parse PROXY protocol header
            if let Ok((header, header_len)) = ProxyHeader::parse(data, ParseConfig::default()) {
                if let Some(proxied_addr) = header.proxied_address() {
                     log::info!("Parsed PROXY protocol header. Original source: {}", proxied_addr.source);
                    source_addr = proxied_addr.source;
                }
               
                data = &data[header_len..];
            }

            let message: Message = match serde_json::from_slice(data) {
                Ok(msg) => msg,
                Err(e) => {
                    log::error!("Failed to deserialize message from {}: {}", source_addr, e);
                    continue;
                }
            };
            log::info!("Received message from {}: {:?}", source_addr, message);

            // If a peer is known, update their source address to the latest one
            let username = get_username_from_message(&message);
            if let Some(name) = &username {
                if let Some(peer_info) = self.peers.get_mut(name) {
                    if peer_info.source_addr != source_addr {
                        log::info!("Updating source address for {} from {} to {}", name, peer_info.source_addr, source_addr);
                        peer_info.source_addr = source_addr;
                    }
                }
            }

            match message {
                Message::Register { username, public_addr } => self.handle_registration(source_addr, username, public_addr).await?,
                Message::Connect {
                    from_username,
                    target_username,
                } => self.handle_connection_request(from_username, target_username).await?,
                Message::SessionFinished { username } => self.handle_session_finished(username).await?,
                Message::RelayMessage {
                    from_username,
                    target_username,
                    content,
                } => {
                    self.handle_relay_message(from_username, target_username, content)
                        .await?
                }
                _ => {}
            }
        }
    }

    async fn handle_relay_message(
        &mut self,
        from_username: String,
        target_username: String,
        content: String,
    ) -> Result<()> {
        if let Some(target_info) = self.peers.get(&target_username) {
            let msg = Message::RelayedMessage {
                from_username,
                content,
            };
            let msg_bytes = serde_json::to_vec(&msg)?;
            // When relaying, we send to the source_addr, as that's the one we can reach.
            self.socket.send_to(&msg_bytes, target_info.source_addr).await?;
            log::info!("Relayed message to {} at {}", target_username, target_info.source_addr);
        } else {
            log::warn!(
                "Could not find target peer {} to relay message",
                target_username
            );
            if let Some(sender_info) = self.peers.get(&from_username) {
                 let response = serde_json::to_vec(&Message::TargetNotFound)?;
                 self.socket.send_to(&response, sender_info.source_addr).await?;
            }
        }
        Ok(())
    }

    async fn handle_registration(&mut self, source_addr: SocketAddr, username: String, public_addr: SocketAddr) -> Result<()> {
        let peer_info = PeerInfo {
            public_addr,
            source_addr,
            status: PeerStatus::Idle,
        };
        self.peers.insert(username.clone(), peer_info);
        log::info!("Registered peer '{}' with public address {} (source: {})", username, public_addr, source_addr);

        let ack = serde_json::to_vec(&Message::RegisterAck)?;
        self.socket.send_to(&ack, source_addr).await?;
        Ok(())
    }

    async fn handle_connection_request(
        &mut self,
        requester_username: String,
        target_username: String,
    ) -> Result<()> {
        let requester_info = self.peers.get(&requester_username).cloned();
        let target_info = self.peers.get(&target_username).cloned();

        let requester_source_addr = if let Some(info) = requester_info.as_ref() {
            info.source_addr
        } else {
            log::warn!("Connection request from unknown user: {}", requester_username);
            return Ok(());
        };

        if let (Some(requester_info), Some(target_info)) = (requester_info, target_info) {
            if target_info.status != PeerStatus::Idle {
                let response = serde_json::to_vec(&Message::TargetBusy)?;
                self.socket.send_to(&response, requester_source_addr).await?;
                return Ok(());
            }

            // Introduce peers by sending them each other's PUBLIC address
            log::info!("Introducing {} ({}) and {} ({})", requester_username, requester_info.public_addr, target_username, target_info.public_addr);

            let msg_to_requester = serde_json::to_vec(&Message::PeerInfo {
                peer: target_info.public_addr,
                username: target_username.clone(),
            })?;
            self.socket
                .send_to(&msg_to_requester, requester_info.source_addr)
                .await?;

            let msg_to_target = serde_json::to_vec(&Message::PeerInfo {
                peer: requester_info.public_addr,
                username: requester_username.clone(),
            })?;
            self.socket.send_to(&msg_to_target, target_info.source_addr).await?;

            // Update statuses
            if let Some(info) = self.peers.get_mut(&target_username) {
                info.status = PeerStatus::Busy(requester_username.clone());
            }
            if let Some(info) = self.peers.get_mut(&requester_username) {
                info.status = PeerStatus::Busy(target_username.clone());
            }
        } else {
            let response = serde_json::to_vec(&Message::TargetNotFound)?;
            self.socket.send_to(&response, requester_source_addr).await?;
        }

        Ok(())
    }

    async fn handle_session_finished(&mut self, username: String) -> Result<()> {
        let other_peer_username = if let Some(peer_info) = self.peers.get(&username) {
            if let PeerStatus::Busy(other) = &peer_info.status {
                Some(other.clone())
            } else {
                None
            }
        } else {
            None
        };

        if let Some(other_username) = other_peer_username {
            if let Some(other_peer_info) = self.peers.get_mut(&other_username) {
                other_peer_info.status = PeerStatus::Idle;
                 log::info!("User {} is now Idle", other_username);
            }
        }

        if let Some(peer_info) = self.peers.get_mut(&username) {
            log::info!("Session finished for {}. User is now Idle.", username);
            peer_info.status = PeerStatus::Idle;
        }
        Ok(())
    }
}

fn get_username_from_message(message: &Message) -> Option<String> {
    match message {
        Message::Register { username, .. } => Some(username.clone()),
        Message::Connect { from_username, .. } => Some(from_username.clone()),
        Message::SessionFinished { username } => Some(username.clone()),
        Message::RelayMessage { from_username, .. } => Some(from_username.clone()),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let mut server = RelayServer::new("0.0.0.0:8080").await?;
    server.run().await
}