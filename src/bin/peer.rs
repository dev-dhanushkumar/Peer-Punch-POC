use anyhow::Result;
use peer_punch::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch, Mutex};
use stunclient::StunClient;

use std::env;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ConnectionStatus {
    Relayed,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PeerSessionStatus {
    Idle,
    Connecting,
    InChat,
}

struct Peer {
    username: String,
    socket: Arc<UdpSocket>,
    relay_addr: SocketAddr,
    public_addr: SocketAddr,
    other_peer_rx: watch::Receiver<Option<(SocketAddr, String)>>,
    other_peer_tx: watch::Sender<Option<(SocketAddr, String)>>,
    connection_status: Arc<Mutex<ConnectionStatus>>,
    session_status: Arc<Mutex<PeerSessionStatus>>,
}

impl Peer {
    async fn new(username: String, relay_addr: SocketAddr, public_addr: SocketAddr, socket: UdpSocket) -> Result<Self> {
        log::info!("Peer listening on {}", socket.local_addr()?);
        let (tx, rx) = watch::channel(None);
        Ok(Self {
            username,
            socket: Arc::new(socket),
            relay_addr,
            public_addr,
            other_peer_rx: rx,
            other_peer_tx: tx,
            connection_status: Arc::new(Mutex::new(ConnectionStatus::Relayed)),
            session_status: Arc::new(Mutex::new(PeerSessionStatus::Idle)),
        })
    }

    async fn run(&mut self) -> Result<()> {
        self.register().await?;
        self.spawn_network_handler();
        self.spawn_keep_alive_handler();

        println!("Ready to connect. Enter a username to connect to, or wait for an incoming connection.");

        let mut reader = BufReader::new(io::stdin());
        let (tx, mut rx) = mpsc::channel(100);

        // Spawn a task to read from stdin
        let input_tx = tx.clone();
        tokio::spawn(async move {
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                    if input_tx.send(line).await.is_err() {
                        break;
                    }
                } else {
                    break;
                }
            }
        });

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    self.shutdown().await?;
                    break;
                }
                Some(line) = rx.recv() => {
                    let current_session_status = *self.session_status.lock().await;
                    if current_session_status == PeerSessionStatus::InChat {
                        self.send_message(line.trim().to_string()).await?;
                    } else if current_session_status == PeerSessionStatus::Idle {
                        let target_username = line.trim().to_string();
                        if !target_username.is_empty() {
                            self.connect_to_peer(target_username).await?;
                        }
                    } else {
                        log::warn!("In the process of connecting, please wait...");
                    }
                }
            }
        }

        Ok(())
    }

    async fn register(&self) -> Result<()> {
        let register_msg = serde_json::to_vec(&Message::Register {
            username: self.username.clone(),
            public_addr: self.public_addr,
        })?;
        self.socket.send_to(&register_msg, self.relay_addr).await?;
        log::info!("Sent registration message to relay server with public address {}", self.public_addr);
        Ok(())
    }

    async fn connect_to_peer(&self, target_username: String) -> Result<()> {
        let mut session_status = self.session_status.lock().await;
        if *session_status != PeerSessionStatus::Idle {
            log::warn!("Cannot connect while already in a session or connecting.");
            return Ok(());
        }
        *session_status = PeerSessionStatus::Connecting;

        let connect_msg = serde_json::to_vec(&Message::Connect {
            from_username: self.username.clone(),
            target_username,
        })?;
        self.socket.send_to(&connect_msg, self.relay_addr).await?;
        log::info!("Sent connection request to relay server");
        Ok(())
    }

    async fn send_message(&self, content: String) -> Result<()> {
        let status = *self.connection_status.lock().await;
        let other_peer = self.other_peer_rx.borrow().clone();

        if let Some((peer_addr, target_username)) = other_peer {
            let msg = if status == ConnectionStatus::Direct {
                log::info!("Sending direct message to {}", peer_addr);
                Message::DirectMessage { content }
            } else {
                log::info!("Sending relayed message for {}", target_username);
                Message::RelayMessage {
                    from_username: self.username.clone(),
                    target_username: target_username.clone(),
                    content,
                }
            };

            let msg_bytes = serde_json::to_vec(&msg)?;
            let addr = if status == ConnectionStatus::Direct {
                peer_addr
            } else {
                self.relay_addr
            };
            self.socket.send_to(&msg_bytes, addr).await?;
        } else {
            log::warn!("No peer to send message to yet.");
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        let session_finished_msg = serde_json::to_vec(&Message::SessionFinished {
            username: self.username.clone(),
        })?;
        self.socket
            .send_to(&session_finished_msg, self.relay_addr)
            .await?;
        println!("Exiting...");
        Ok(())
    }

    fn spawn_network_handler(&self) -> tokio::task::JoinHandle<()> {
        let socket_clone = self.socket.clone();
        let other_peer_tx = self.other_peer_tx.clone();
        let connection_status = self.connection_status.clone();
        let session_status = self.session_status.clone();

        tokio::spawn(async move {
            let mut buf = [0; 1024];
            loop {
                let (len, addr) = match socket_clone.recv_from(&mut buf).await {
                    Ok(res) => res,
                    Err(e) => {
                        log::error!("Error receiving from socket: {}", e);
                        continue;
                    }
                };
                let message: Message = match serde_json::from_slice(&buf[..len]) {
                    Ok(msg) => msg,
                    Err(e) => {
                        log::warn!("Failed to deserialize message from {}: {}", addr, e);
                        continue;
                    }
                };

                log::info!("Received message from {}: {:?}", addr, message);

                match message {
                    Message::RegisterAck => {
                        log::info!("Successfully registered with relay server.");
                    }
                    Message::PeerInfo { peer, username } => {
                        let mut sess_status = session_status.lock().await;
                        if *sess_status == PeerSessionStatus::InChat {
                            log::warn!("Ignoring PeerInfo, already in a chat.");
                            continue;
                        }
                        *sess_status = PeerSessionStatus::InChat;

                        println!("---");
                        println!("Connected to {}. You can now chat.", username);
                        println!("---");

                        other_peer_tx.send(Some((peer, username.clone()))).unwrap();
                        
                        let punch_msg = serde_json::to_vec(&Message::Ping).unwrap();
                        socket_clone.send_to(&punch_msg, peer).await.unwrap();
                    }
                    Message::RelayedMessage { from_username, content } => {
                        println!("[{}] (Relayed)> {}", from_username, content);
                    }
                    Message::DirectMessage { content } => {
                        if let Some((_, ref username)) = *other_peer_tx.borrow() {
                            println!("[{}] (Direct)> {}", username, content);
                        }
                    }
                    Message::Ping => {
                        let pong = serde_json::to_vec(&Message::Pong).unwrap();
                        socket_clone.send_to(&pong, addr).await.unwrap();
                    }
                    Message::Pong => {
                        log::info!("Received pong from {}. Connection is now DIRECT.", addr);
                        let mut status = connection_status.lock().await;
                        if *status != ConnectionStatus::Direct {
                            *status = ConnectionStatus::Direct;
                            println!("--- Connection is now Direct ---");
                        }
                    }
                    Message::Goodbye => {
                        if let Some((_, ref username)) = *other_peer_tx.borrow() {
                            println!("[{}] has left the chat.", username);
                        }
                        other_peer_tx.send(None).unwrap();
                        *session_status.lock().await = PeerSessionStatus::Idle;
                        *connection_status.lock().await = ConnectionStatus::Relayed;
                    }
                    Message::TargetNotFound => {
                        println!("Target user not found.");
                        *session_status.lock().await = PeerSessionStatus::Idle;
                    }
                    Message::TargetBusy => {
                        println!("Target user is busy.");
                        *session_status.lock().await = PeerSessionStatus::Idle;
                    }
                    _ => {}
                }
            }
        })
    }

    fn spawn_keep_alive_handler(&self) -> tokio::task::JoinHandle<()> {
        let socket_clone = self.socket.clone();
        let other_peer_rx = self.other_peer_rx.clone();
        let connection_status = self.connection_status.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;

                let peer_info = other_peer_rx.borrow().clone();
                if peer_info.is_none() {
                    continue;
                }
                
                if let Some((peer_addr, _)) = peer_info {
                    let status = *connection_status.lock().await;
                    if status == ConnectionStatus::Direct {
                        log::info!("Sending direct ping to {}", peer_addr);
                        let ping = serde_json::to_vec(&Message::Ping).unwrap();
                        if let Err(e) = socket_clone.send_to(&ping, peer_addr).await {
                            log::error!("Failed to send keep-alive ping: {}", e);
                        }
                    }
                }
            }
        })
    }
}

async fn get_public_addr(stun_server: &str) -> Result<(SocketAddr, UdpSocket)> {
    let stun_addr = tokio::net::lookup_host(stun_server)
        .await?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve STUN server address to an IPv4 address"))?;

    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    log::info!("Bound to local address: {}", socket.local_addr()?);

    let client = StunClient::new(stun_addr);
    let external_addr = client.query_external_address_async(&socket).await?;
    
    log::info!("Got public address from STUN server: {}", external_addr);
    Ok((external_addr, socket))
}


#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <relay_server_addr> <stun_server_addr>", args[0]);
        return Ok(());
    }
    let relay_addr_str = &args[1];
    let stun_server_str = &args[2];

    let relay_addr = tokio::net::lookup_host(relay_addr_str)
        .await?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve relay server address to an IPv4 address"))?;

    println!("Please enter your username:");
    let mut username = String::new();
    let mut reader = BufReader::new(io::stdin());
    reader.read_line(&mut username).await?;
    let username = username.trim().to_string();

    let (public_addr, socket) = get_public_addr(stun_server_str).await?;

    let mut peer = Peer::new(username, relay_addr, public_addr, socket).await?;
    peer.run().await
}
