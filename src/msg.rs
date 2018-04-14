use bincode;
use control;
use serde;
use std;
use std::net::SocketAddrV4;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Hash, Eq)]
pub struct Username(pub String);

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
pub enum Warning {
    FailedToCompress,
    FailedToDecompress,
    FailedToDeserialize(Box<bincode::ErrorKind>),
    IoFailure(std::io::Error),
    FailedToSerialize(Box<bincode::ErrorKind>),
    FailedToSerializeAck(Box<bincode::ErrorKind>),
    WrongMessageType,
    UnknownSource(std::net::SocketAddr),
}

/// When a CommError is of this type, a servicer will drop a client.
#[derive(Debug)]
pub enum Drop {
    AlreadyConnected,
    ApplicationThreadDisconnected,
    FailedToAuthenticate(Credentials),
    FailedToBind(std::io::Error),
    FailedToConnect(std::io::Error),
    IoFailure(std::io::Error),
}

#[derive(Debug)]
pub enum CommError {
    Warning(Warning),
    Drop(Drop),
    Exit,
}

impl From<std::io::Error> for CommError {
    fn from(err: std::io::Error) -> CommError {
        match err.kind() {
            std::io::ErrorKind::NotFound => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::ConnectionReset => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::WouldBlock => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::InvalidInput => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::InvalidData => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::WriteZero => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::Interrupted => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::Other => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::UnexpectedEof => CommError::Warning(Warning::IoFailure(err)),
            std::io::ErrorKind::PermissionDenied => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::ConnectionRefused => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::ConnectionAborted => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::NotConnected => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::AddrInUse => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::AddrNotAvailable => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::BrokenPipe => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::AlreadyExists => CommError::Drop(Drop::IoFailure(err)),
            std::io::ErrorKind::TimedOut => CommError::Drop(Drop::IoFailure(err)),
            _ => CommError::Drop(Drop::IoFailure(err)),
        }
    }
}

pub mod reliable {
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
    pub enum ClientMessage {
        /// A message to send to the server that will be visible to all players.
        ChatMessage(String),
        /// Request to join the server with the given user's `Credentials`.
        JoinRequest(super::Credentials),
    }

    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub struct ChatMessage {
        from: super::Username,
        message: String,
    }

    /// Represents messages passed from the server to a client over a reliable (Tcp) transport.
    #[derive(Serialize, Deserialize, Clone)]
    pub enum ServerMessage {
        JoinResponse(JoinResponse),
        ChatMessage(ChatMessage),
        /// World tick number for the latest client controller input received.
        LastTickReceived(usize),
    }
}

pub mod low_latency {
    use super::*;

    /// Represents messages a client can send to the server over a low-latency (Udp) transport.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub enum ClientMessage {
        /// A compressed series of control inputs -- one for each game tick since the last tick
        /// received.
        ControllerInput(control::CompressedControllerSequence),
    }

    /// Represents messages passed from the server to a client over a low-latency (Udp) transport.
    #[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
    pub enum ServerMessage<StateT>
    where
        StateT: serde::Serialize,
    {
        /// The latest segment of world state needed by a specific client.
        WorldState(StateT),
    }
}

macro_rules! comm_log {
    ($x:expr) => (
        match $x {
            msg::CommError::Warning(warn) => { warn!("{:?}", warn); },
            msg::CommError::Drop(drop) => { warn!("{:?}", drop); },
            msg::CommError::Exit => { error!("Exit"); },
        });
}

macro_rules! log_and_exit {
    ($x:expr) => (
        comm_log!($x);
        return;
    );
}
