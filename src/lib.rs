use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    // Client to Server
    Register {
        username: String,
        public_addr: SocketAddr,
    },
    Connect {
        from_username: String,
        target_username: String,
    },
    SessionFinished {
        username: String,
    },
    RelayMessage {
        from_username: String,
        target_username: String,
        content: String,
    },

    // Server to Client
    RegisterAck,
    PeerInfo {
        peer: SocketAddr,
        username: String,
    },
    TargetNotFound,
    TargetBusy,
    RelayedMessage {
        from_username: String,
        content: String,
    },

    // Peer to Peer
    DirectMessage {
        content: String,
    },
    Ping,
    Pong,
    Goodbye,
}
