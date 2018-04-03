use control;
use msg;
use std;
use bincode;
use serde;
use std::net::{TcpStream, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std::io::Read;

mod reliable;
mod low_latency;

#[derive(Clone)]
pub enum ServicerMessage<StateT> {
    /// The latest segment of world state needed by a specific client.
    WorldState(StateT),
    /// A message that will be visible to all players.
    ChatMessage(msg::reliable::ChatMessage),
    /// Implies there is no message to send.
    None,
}
