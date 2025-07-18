use anyhow::Result;
use peer_punch::Message;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use std::env;

// We need to track if we have a direct connection
#[derive(Debug, Clone, Copy, PartialEq)]
enum ConnectionStatus {
    Relayed,
    Direct,
}

struct Peer {
    username: String,
    socket: Arc<UdpSocket>,
    relay_addr: SocketAddr,
    other_peer_rx: watch::Receiver<Option<(SocketAddr, String)>>,
    other_peer_tx: watch::Sender<Option<(SocketAddr, String)>>,
    connection_status: Arc<Mutex<ConnectionStatus>>,
}

impl Peer {
    async fn new(username: String, relay_addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        log::info!("Peer listening on {}", socket.local_addr()?);
        let (tx, rx) = watch::channel(None);
        Ok(Self {
            username,
            socket: Arc::new(socket),
            relay_addr,
            other_peer_rx: rx,
            other_peer_tx: tx,
            connection_status: Arc::new(Mutex::new(ConnectionStatus::Relayed)),
        })
    }

    async fn run(&mut self) -> Result<()> {
        self.register().await?;

        println!("Enter the username of the peer you want to connect to: ");
        let mut target_username = String::new();
        let mut reader = BufReader::new(io::stdin());
        reader.read_line(&mut target_username).await?;
        let target_username = target_username.trim().to_string();
        self.connect_to_peer(target_username).await?;

        let (tx, mut rx) = mpsc::channel(10);

        self.spawn_input_handler(tx);
        self.spawn_network_handler();
        self.spawn_keep_alive_handler();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                self.shutdown().await?;
            }
            _ = async {
                while let Some(line) = rx.recv().await {
                    self.send_message(line).await?;
                }
                Result::<(), anyhow::Error>::Ok(())
            } => {}
        }

        Ok(())
    }

    async fn register(&self) -> Result<()> {
        let register_msg = serde_json::to_vec(&Message::Register {
            username: self.username.clone(),
        })?;
        self.socket.send_to(&register_msg, self.relay_addr).await?;
        log::info!("Sent registration message to relay server");
        Ok(())
    }

    async fn connect_to_peer(&self, target_username: String) -> Result<()> {
        let connect_msg = serde_json::to_vec(&Message::Connect {
            from_username: self.username.clone(),
            target_username,
        })?;
        self.socket.send_to(&connect_msg, self.relay_addr).await?;
        log::info!("Sent connection request to relay server");
        Ok(())
    }

    async fn send_message(&self, content: String) -> Result<()> {
        let status = *self.connection_status.lock().unwrap();
        if let Some((peer_addr, target_username)) = &*self.other_peer_rx.borrow() {
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
                *peer_addr
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

    fn spawn_input_handler(&self, tx: mpsc::Sender<String>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut reader = BufReader::new(io::stdin());
            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap() > 0 {
                tx.send(line.trim().to_string()).await.unwrap();
                line.clear();
            }
        })
    }

    fn spawn_network_handler(&self) -> tokio::task::JoinHandle<()> {
        let socket_clone = self.socket.clone();
        let other_peer_tx = self.other_peer_tx.clone();
        let _relay_addr = self.relay_addr;
        let connection_status = self.connection_status.clone();

        tokio::spawn(async move {
            let mut buf = [0; 1024];
            loop {
                let (len, addr) = socket_clone.recv_from(&mut buf).await.unwrap();
                let message: Message = serde_json::from_slice(&buf[..len]).unwrap();
                log::info!("Received message from {}: {:?}", addr, message);

                match message {
                    Message::RegisterAck => {
                        log::info!("Successfully registered with relay server.");
                    }
                    Message::PeerInfo { peer, username } => {
                        log::info!("Received peer info for {}: {}. Sending punch.", username, peer);
                        other_peer_tx.send(Some((peer, username.clone()))).unwrap();
                        
                        // Punch a hole to the peer's public address
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
                        let mut status = connection_status.lock().unwrap();
                        *status = ConnectionStatus::Direct;
                    }
                    Message::Goodbye => {
                        if let Some((_, ref username)) = *other_peer_tx.borrow() {
                            println!("[{}] has left the chat.", username);
                        }
                        other_peer_tx.send(None).unwrap();
                    }
                    Message::TargetNotFound => {
                        println!("Target user not found.");
                    }
                    Message::TargetBusy => {
                        println!("Target user is busy.");
                    }
                    _ => {}
                }
            }
        })
    }

    fn spawn_keep_alive_handler(&self) -> tokio::task::JoinHandle<()> {
        let socket_clone = self.socket.clone();
        let mut other_peer_rx = self.other_peer_rx.clone();
        let connection_status = self.connection_status.clone();

        tokio::spawn(async move {
            loop {
                // Wait until we have a peer
                if other_peer_rx.borrow().is_none() {
                    other_peer_rx.changed().await.unwrap();
                    continue;
                }
                
                let (peer_addr, _) = other_peer_rx.borrow().clone().unwrap();
                
                tokio::time::sleep(Duration::from_secs(5)).await;

                let status = *connection_status.lock().unwrap();
                if status == ConnectionStatus::Direct {
                    log::info!("Sending direct ping to {}", peer_addr);
                    let ping = serde_json::to_vec(&Message::Ping).unwrap();
                    socket_clone.send_to(&ping, peer_addr).await.unwrap();
                }
            }
        })
    }
}

use tokio::net::lookup_host;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <relay_server_addr>", args[0]);
        return Ok(());
    }
    let relay_addr_str = args[1].strip_prefix("https://").unwrap_or(&args[1]);

    let relay_addr = lookup_host(relay_addr_str)
        .await?
        .find(|addr| addr.is_ipv4())
        .ok_or_else(|| anyhow::anyhow!("Could not resolve relay server address to an IPv4 address"))?;

    println!("Please enter your username:");
    let mut username = String::new();
    let mut reader = BufReader::new(io::stdin());
    reader.read_line(&mut username).await?;
    let username = username.trim().to_string();

    let mut peer = Peer::new(username, relay_addr).await?;
    peer.run().await
}