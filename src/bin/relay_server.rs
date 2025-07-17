use anyhow::Result;
use peer_punch::Message;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

#[derive(Debug, Clone, PartialEq)]
enum PeerStatus {
    Idle,
    Busy(SocketAddr),
}

#[derive(Debug, Clone)]
struct PeerInfo {
    username: String,
    status: PeerStatus,
}

struct RelayServer {
    socket: UdpSocket,
    peers: HashMap<SocketAddr, PeerInfo>,
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
            let (len, addr) = self.socket.recv_from(&mut buf).await?;
            let message: Message = serde_json::from_slice(&buf[..len])?;
            log::info!("Received message from {}: {:?}", addr, message);

            match message {
                Message::Register { username } => self.handle_registration(addr, username).await?,
                Message::Connect { target_username } => self.handle_connection_request(addr, target_username).await?,
                Message::SessionFinished => self.handle_session_finished(addr).await?,
                _ => {}
            }
        }
    }

    async fn handle_registration(&mut self, addr: SocketAddr, username: String) -> Result<()> {
        let peer_info = PeerInfo { username: username.clone(), status: PeerStatus::Idle };
        self.peers.insert(addr, peer_info);
        log::info!("Registered peer: {} as {}", addr, username);

        let ack = serde_json::to_vec(&Message::RegisterAck)?;
        self.socket.send_to(&ack, addr).await?;
        Ok(())
    }

    async fn handle_connection_request(&mut self, requester_addr: SocketAddr, target_username: String) -> Result<()> {
        let requester_info = self.peers.get(&requester_addr).cloned();
        let target_peer = self.peers.iter()
            .find(|(_, info)| info.username == target_username)
            .map(|(addr, info)| (*addr, info.clone()));

        if let (Some(requester_info), Some((target_addr, target_info))) = (requester_info, target_peer) {
            if target_info.status != PeerStatus::Idle {
                let response = serde_json::to_vec(&Message::TargetBusy)?;
                self.socket.send_to(&response, requester_addr).await?;
                return Ok(());
            }

            // Introduce peers
            let msg_to_requester = serde_json::to_vec(&Message::PeerInfo { peer: target_addr, username: target_info.username.clone() })?;
            self.socket.send_to(&msg_to_requester, requester_addr).await?;

            let msg_to_target = serde_json::to_vec(&Message::PeerInfo { peer: requester_addr, username: requester_info.username.clone() })?;
            self.socket.send_to(&msg_to_target, target_addr).await?;

            // Update statuses
            if let Some(info) = self.peers.get_mut(&target_addr) {
                info.status = PeerStatus::Busy(requester_addr);
            }
            if let Some(info) = self.peers.get_mut(&requester_addr) {
                info.status = PeerStatus::Busy(target_addr);
            }

            log::info!("Introduced {} and {}", requester_addr, target_addr);
        } else {
            let response = serde_json::to_vec(&Message::TargetNotFound)?;
            self.socket.send_to(&response, requester_addr).await?;
        }

        Ok(())
    }

    async fn handle_session_finished(&mut self, addr: SocketAddr) -> Result<()> {
        if let Some(peer_info) = self.peers.get_mut(&addr) {
            log::info!("Session finished for {}", peer_info.username);
            peer_info.status = PeerStatus::Idle;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let mut server = RelayServer::new("127.0.0.1:8080").await?;
    server.run().await
}
