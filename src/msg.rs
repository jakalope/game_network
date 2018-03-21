use bincode;
use controller_sequence as ctrl_seq;
use serde;
use std;
use std::net::SocketAddrV4;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Hash, Eq)]
pub struct Username(String);

/// Data owned by the server about a client.
#[derive(Clone)]
pub struct ClientData {
    /// This user's in-game nickname.
    pub username: Username,

    /// Client's socket address for low-latency transport. This is where the server will send
    /// low-latency data for this client.
    pub udp_addr: SocketAddrV4,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, Eq)]
pub struct Credentials {
    pub username: Username,
    pub password: String,

    /// When requesting to join the server via Tcp, the client must supply an open Udp port.
    pub udp_addr: SocketAddrV4,
}

#[derive(Debug)]
pub enum CommError {
    AlreadyConnected,
    ApplicationThreadDisconnected,
    FailedToAuthenticate(Credentials),
    FailedToBind(std::io::Error),
    FailedToCompress,
    FailedToConnect(std::io::Error),
    FailedToDecompress,
    FailedToDeserialize(Box<bincode::ErrorKind>),
    FailedToRead(std::io::Error),
    FailedToReceive(std::io::Error),
    FailedToSend(std::io::Error),
    FailedToSendAck(std::io::Error),
    FailedToSerialize(Box<bincode::ErrorKind>),
    FailedToSerializeAck(Box<bincode::ErrorKind>),
    UnknownSource(std::net::SocketAddr),
    WrongMessageType,
}

/// A server's response to a `ClientMessage::JoinRequest`. If a `JoinRequest` was rejected, a
/// reason will be provided. Otherwise, a simple `Confirmation` is sent before world states begin
/// streaming.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum JoinResponse {
    Confirmation,
    AuthenticationError,
}

/// Represents messages a client can send to the server over a reliable (Tcp) transport.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientReliableMessage {
    /// A message to send to the server that will be visible to all players.
    ChatMessage(String),
    /// Request to join the server with the given user's `Credentials`.
    JoinRequest(Credentials),
    /// Implies there is no message to send.
    None,
}


/// Represents messages a client can send to the server over a low-latency (Udp) transport.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientLowLatencyMessage {
    /// A compressed series of control inputs -- one for each game tick since the last tick
    /// received.
    ControllerInput(ctrl_seq::CompressedControllerSequence),
    /// Implies there is no message to send.
    None,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChatMessage {
    from: Username,
    message: String,
}

/// Represents messages passed from the server to a client over a reliable (Tcp) transport.
#[derive(Serialize, Deserialize, Clone)]
pub enum ServerReliableMessage {
    JoinResponse(JoinResponse),
    ChatMessage(ChatMessage),
    /// World tick number for the latest client controller input received.
    LastTickReceived(usize),
    /// Implies there is no message to send.
    None,
}

/// Represents messages passed from the server to a client over a low-latency (Udp) transport.
#[derive(Serialize, Deserialize, Clone)]
pub enum ServerLowLatencyMessage<StateT>
where
    StateT: serde::Serialize,
{
    /// The latest segment of world state needed by a specific client.
    WorldState(StateT),
    /// Implies there is no message to send.
    None,
}

#[derive(Clone)]
pub struct ToClient<StateT>
where
    StateT: serde::Serialize,
{
    pub to: Username,
    pub payload: ServerLowLatencyMessage<StateT>,
}

/// Represents a message passed from the server's application thread to its low-latency servicer
/// thread.
#[derive(Clone)]
pub enum ApplicationMessage<StateT>
where
    StateT: serde::Serialize,
{
    /// A message to be forwarded to a specific client.
    ToClient(ToClient<StateT>),

    /// When a new client connects, the low latency servicer needs to update its user/socket map.
    NewClient(ClientData),

    /// When a client disconnects, the low latency servicer needs to update its user/socket map.
    ClientDisconnect(Username),
}

#[derive(Clone)]
pub enum ServicerPayload {
    /// A message that will be visible to all players.
    ChatMessage(String),
    /// A series of control inputs -- one for each game tick since the last tick received.
    ControllerSequence(ctrl_seq::ControllerSequence),
    ///
    ClientData(ClientData),
    /// Implies there is no message to send.
    None,
}

/// Represents messages passed from client servicer threads to the main application thread.
#[derive(Clone)]
pub struct ServicerMessage {
    /// Originator's user name.
    pub from: Username,
    /// The message payload to be handled by the application thread.
    pub payload: ServicerPayload,
}

#[derive(Clone)]
pub enum ClientInProcMessage<StateT> {
    /// The latest segment of world state needed by a specific client.
    WorldState(StateT),
    /// A message that will be visible to all players.
    ChatMessage(ChatMessage),
    /// Implies there is no message to send.
    None,
}
