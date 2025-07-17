# Peer-Punch

A simple peer-to-peer (P2P) chat application written in Rust that demonstrates UDP hole punching. This project allows two clients, both potentially behind NATs, to establish a direct connection with each other for communication, using a central relay server for the initial introduction.

## Features

*   **UDP Hole Punching:** Establishes direct P2P connections between clients behind NATs.
*   **Relay Server:** A simple server that introduces peers to each other.
*   **Peer-to-Peer Chat:** Once connected, clients can chat directly with each other.
*   **Usernames:** Users can choose a username for the chat session.
*   **Keep-Alive Mechanism:** Periodically sends ping messages to keep the NAT pinhole open.
*   **Graceful Shutdown:** Peers notify each other when they are leaving the chat.
*   **Connect by Username:** Users can connect to a specific peer by their username.

## How It Works

1.  **Registration:** Both clients register with the relay server, providing their username.
2.  **Connection Request:** One client requests to connect to another client by their username.
3.  **Introduction:** The relay server sends the public IP address and port of each client to the other.
4.  **Hole Punching:** Both clients send a message to the other client's public address. This "punches a hole" in their respective NATs, allowing direct communication.
5.  **Direct Connection:** The clients can now communicate directly with each other without the need for the relay server.

## Getting Started

### Prerequisites

*   [Rust](https://www.rust-lang.org/tools/install)

### Build

Clone the repository and build the project:

```bash
git clone https://github.com/your-username/Peer-Punch.git
cd Peer-Punch
cargo build
```

### Run

1.  **Start the Relay Server:**

    ```bash
    cargo run --bin relay_server
    ```

2.  **Start the First Peer Client:**

    Open a new terminal and run:

    ```bash
    cargo run --bin peer
    ```

    Enter a username when prompted.

3.  **Start the Second Peer Client:**

    Open another new terminal and run:

    ```bash
    cargo run --bin peer
    ```

    Enter a different username.

## Usage

1.  After starting both peer clients, one client will be prompted to enter the username of the peer they want to connect to.
2.  Once the connection is established, you can type messages in either peer's terminal, and they will appear in the other peer's terminal.
3.  To exit, press `Ctrl+C`.

## Messaging Protocol

The communication between the clients and the server is done using a simple JSON-based messaging protocol. The messages are defined in `src/lib.rs`.

### Client to Server

*   `Register { username: String }`: Registers the client with the relay server.
*   `Connect { target_username: String }`: Requests to connect to a specific user.
*   `SessionFinished`: Informs the server that the chat session has ended.

### Server to Client

*   `RegisterAck`: Acknowledges the registration.
*   `PeerInfo { peer: SocketAddr, username: String }`: Provides the address and username of the other peer.
*   `TargetNotFound`: Indicates that the target user could not be found.
*   `TargetBusy`: Indicates that the target user is already in a chat session.

### Peer to Peer

*   `DirectMessage { content: String }`: A chat message.
*   `Ping`: A keep-alive message.
*   `Pong`: The response to a `Ping`.
*   `Goodbye`: A message indicating that the peer is leaving the chat.

## Future Improvements

*   **Encryption:** Encrypt the communication between peers.
*   **File Transfer:** Allow peers to send files to each other.
*   **Multiple Peers:** Allow more than two peers to join a chat session.
*   **GUI:** Create a graphical user interface for the chat client.
