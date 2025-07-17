use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    // Client to Server
    Register { username: String },
    Connect { target_username: String },
    SessionFinished,

    // Server to Client
    RegisterAck,
    PeerInfo { peer: SocketAddr, username: String },
    TargetNotFound,
    TargetBusy,

    // Peer to Peer
    DirectMessage { content: String },
    Ping,
    Pong,
    Goodbye,
}
