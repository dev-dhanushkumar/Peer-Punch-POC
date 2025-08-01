use anyhow::Result;
use peer_punch::Message;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

struct Peer {
    username: String,
    socket: Arc<UdpSocket>,
    relay_addr: SocketAddr,
    other_peer_rx: watch::Receiver<Option<(SocketAddr, String)>>,
    other_peer_tx: watch::Sender<Option<(SocketAddr, String)>>,
}

impl Peer {
    async fn new(username: String, relay_addr: &str) -> Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        log::info!("Peer listening on {}", socket.local_addr()?);
        let (tx, rx) = watch::channel(None);
        Ok(Self {
            username,
            socket: Arc::new(socket),
            relay_addr: relay_addr.parse()?,
            other_peer_rx: rx,
            other_peer_tx: tx,
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
                    self.send_direct_message(line).await?;
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
        let connect_msg = serde_json::to_vec(&Message::Connect { target_username })?;
        self.socket.send_to(&connect_msg, self.relay_addr).await?;
        log::info!("Sent connection request to relay server");
        Ok(())
    }

    async fn send_direct_message(&self, content: String) -> Result<()> {
        if let Some((peer_addr, _)) = *self.other_peer_rx.borrow() {
            let msg = Message::DirectMessage { content };
            let msg_bytes = serde_json::to_vec(&msg)?;
            self.socket.send_to(&msg_bytes, peer_addr).await?;
        } else {
            log::warn!("No peer to send message to yet.");
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        if let Some((peer_addr, _)) = *self.other_peer_rx.borrow() {
            let goodbye_msg = serde_json::to_vec(&Message::Goodbye)?;
            self.socket.send_to(&goodbye_msg, peer_addr).await?;
            let session_finished_msg = serde_json::to_vec(&Message::SessionFinished)?;
            self.socket
                .send_to(&session_finished_msg, self.relay_addr)
                .await?;
        }
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
        let relay_addr = self.relay_addr;

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
                        log::info!("Received peer info: {} as {}", peer, username);
                        other_peer_tx.send(Some((peer, username.clone()))).unwrap();
                        // Punch a hole
                        let direct_msg = serde_json::to_vec(&Message::DirectMessage {
                            content: "Punch!".to_string(),
                        })
                        .unwrap();
                        socket_clone.send_to(&direct_msg, peer).await.unwrap();
                    }
                    Message::DirectMessage { content } => {
                        if let Some((_, ref username)) = *other_peer_tx.borrow() {
                            println!("[{}]> {}", username, content);
                        }
                    }
                    Message::Ping => {
                        let pong = serde_json::to_vec(&Message::Pong).unwrap();
                        socket_clone.send_to(&pong, addr).await.unwrap();
                    }
                    Message::Pong => {
                        log::info!("Received pong from {}", addr);
                    }
                    Message::Goodbye => {
                        if let Some((_, ref username)) = *other_peer_tx.borrow() {
                            println!("[{}] has left the chat.", username);
                        }
                        other_peer_tx.send(None).unwrap();
                        let session_finished_msg =
                            serde_json::to_vec(&Message::SessionFinished).unwrap();
                        socket_clone
                            .send_to(&session_finished_msg, relay_addr)
                            .await
                            .unwrap();
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

        tokio::spawn(async move {
            loop {
                other_peer_rx.changed().await.unwrap();
                if let Some((peer_addr, _)) = *other_peer_rx.borrow() {
                    let socket = socket_clone.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(20)).await;
                            let ping = serde_json::to_vec(&Message::Ping).unwrap();
                            socket.send_to(&ping, peer_addr).await.unwrap();
                        }
                    });
                }
            }
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    println!("Please enter your username:");
    let mut username = String::new();
    let mut reader = BufReader::new(io::stdin());
    reader.read_line(&mut username).await?;
    let username = username.trim().to_string();

    let mut peer = Peer::new(username, "127.0.0.1:8080").await?;
    peer.run().await
}
