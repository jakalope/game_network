extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate bincode;

mod bitvec;
mod controller_sequence;

use std::sync::mpsc;
use std::net::{SocketAddr, UdpSocket};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, Eq)]
pub struct Credentials {
    username: String,
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug, Hash, Eq)]
pub struct ClientId(usize);

pub enum SendError {
    FailedToCompress,
    FailedToSerialize(Box<bincode::ErrorKind>),
    FailedToSend(std::io::Error),
}

pub enum RecvError {
    FailedToReceive(std::io::Error),
    FailedToDeserialize(Box<bincode::ErrorKind>),
    FailedToDecompress,
    FailedToSerializeAck(Box<bincode::ErrorKind>),
    FailedToSendAck(std::io::Error),
}

/// A server's response to a `ClientMessage::JoinRequest`. If a `JoinRequest` was rejected, a
/// reason will be provided. Otherwise, a simple `Confirmation` is sent before world states begin
/// streaming.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum JoinResponse {
    RejctionReason(String),
    Confirmation,
}

/// Represents all possible message types a `Client` can send a `Server`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// A message to send to the server that will be visible to all players.
    ChatMessage(String),

    /// Request to join the server with the given user's `Credentials`.
    JoinRequest(Credentials),

    /// A compressed series of control inputs -- one for each game tick since the last
    /// `ServerMessage::LastTickReceived` value.
    ControllerInput(controller_sequence::CompressedControllerSequence),
}

/// Represents all possible message types a `Server` can send a `Client`.
#[derive(Serialize, Deserialize, Clone)]
pub enum ServerMessage<StateT>
where
    StateT: serde::Serialize,
{
    LastTickReceived(usize),
    ChatMessage { username: String, message: String },
    JoinResponse(JoinResponse),
    WorldState(StateT),
}

pub struct Client<StateT> {
    socket: UdpSocket,
    server_addr: SocketAddr,
    self_addr: SocketAddr,
    controller_seq: controller_sequence::ControllerSequence,
    state_sender: mpsc::Sender<StateT>,
}

pub struct Server {
    socket: UdpSocket,
    self_addr: SocketAddr,
    client_inputs: HashMap<ClientId, controller_sequence::ControllerSequence>,
}

impl<StateT> Client<StateT> {
    fn connect(&mut self) -> std::io::Result<()> {
        self.socket = UdpSocket::bind(self.self_addr)?;
        self.socket.connect(self.server_addr)
    }

    pub fn send(&self) -> Result<(), SendError> {
        if !self.controller_seq.is_empty() {
            let payload = self.controller_seq.to_compressed().ok_or(
                SendError::FailedToCompress,
            )?;

            let controller_input = ClientMessage::ControllerInput(payload);

            let encoded: Vec<u8> = bincode::serialize(&controller_input).map_err(|err| {
                SendError::FailedToSerialize(err)
            })?;

            self.socket.send_to(&encoded, self.server_addr).map_err(
                |err| {
                    SendError::FailedToSend(err)
                },
            )?;
        }

        Ok(())
    }

    fn handle_last_tick_received(&mut self, last_tick: usize) -> Result<(), RecvError> {
        // Now we remove the controller inputs the server has ACK'd.
        self.controller_seq.remove_till_tick(last_tick + 1);
        Ok(())
    }

    fn handle_join_response(&self, response: JoinResponse) -> Result<(), RecvError> {
        Ok(())
    }

    fn handle_world_state<'de>(&mut self, state: StateT) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        Ok(())
    }

    fn handle_chat_message(&mut self, user: String, msg: String) -> Result<(), RecvError> {
        Ok(())
    }

    pub fn receive(&mut self) -> Result<(), RecvError>
    where
        StateT: serde::de::DeserializeOwned,
        StateT: serde::Serialize,
    {
        // Receive ACK only from the connected server.
        let mut buf = Vec::<u8>::new();
        self.socket.recv(&mut buf).map_err(|err| {
            RecvError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        // The ACK contains the latest controller input the server has received from us.
        let server_message: ServerMessage<StateT> = bincode::deserialize(&buf[..]).map_err(|err| {
            RecvError::FailedToDeserialize(err)
        })?;

        match server_message {
            ServerMessage::LastTickReceived(tick) => self.handle_last_tick_received(tick),
            ServerMessage::JoinResponse(response) => self.handle_join_response(response),
            ServerMessage::WorldState(state) => self.handle_world_state(state),
            ServerMessage::ChatMessage {
                username: user,
                message: msg,
            } => self.handle_chat_message(user, msg),
        }
    }
}

impl<'de> Server {
    fn bind(&mut self) -> std::io::Result<()> {
        self.socket = UdpSocket::bind(self.self_addr)?;
        Ok(())
    }

    fn handle_controller<StateT>(
        &mut self,
        input: controller_sequence::CompressedControllerSequence,
        src: SocketAddr,
    ) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            RecvError::FailedToDecompress,
        )?;

        // Compute game tick of last controller input received.
        let last_tick: ServerMessage<StateT> =
            ServerMessage::LastTickReceived(controller_seq.last_tick());

        // TODO get client_id from src.
        // Add or update the inputs in the client inputs map.
        self.client_inputs.insert(ClientId(0), controller_seq);

        // Serialize the tick.
        let encode = bincode::serialize(&last_tick).map_err(|err| {
            RecvError::FailedToSerializeAck(err)
        })?;

        // Send the tick.
        self.socket.send_to(&encode, src).map_err(|err| {
            RecvError::FailedToSendAck(err)
        })?;

        Ok(())
    }

    fn handle_join_request<StateT>(
        &mut self,
        cred: Credentials,
        src: SocketAddr,
    ) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {

        // TODO

        Ok(())
    }

    fn handle_chat_message<StateT>(&mut self, msg: String, src: SocketAddr) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {

        // TODO

        Ok(())
    }

    pub fn receive<StateT>(&mut self) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {

        // Receive inputs from anyone.
        let mut buf = Vec::<u8>::new();
        let (_, src) = self.socket.recv_from(&mut buf).map_err(|err| {
            RecvError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        let client_message: ClientMessage = bincode::deserialize(&buf).map_err(|err| {
            RecvError::FailedToDeserialize(err)
        })?;

        match client_message {
            ClientMessage::ControllerInput(input) => self.handle_controller::<StateT>(input, src),
            ClientMessage::JoinRequest(cred) => self.handle_join_request::<StateT>(cred, src),
            ClientMessage::ChatMessage(msg) => self.handle_chat_message::<StateT>(msg, src),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bitvec_empty() {
        // Tests the requirement that all bitvecs must have at least one storage element.
        let bitvec = BitVec::new();
        assert_eq!(0, bitvec.len());
        assert!(bitvec.is_empty());
    }

    #[test]
    fn bitvec_not_empty() {
        let mut bitvec = BitVec::new();
        bitvec.push(true);
        assert_eq!(1, bitvec.len());
        assert_eq!(false, bitvec.is_empty());
    }

    #[test]
    fn bitvec_gt_32() {
        let mut bitvec = BitVec::new();
        for i in 0..33 {
            bitvec.push((i % 2) == 1);
        }
        for i in 0..33 {
            assert_eq!(Some((i % 2) == 1), bitvec.get(i));
        }
        assert_eq!(33, bitvec.len());
        assert_eq!(None, bitvec.get(33));
    }

    #[test]
    fn bitvec_push_get() {
        let mut bitvec = BitVec::new();
        assert_eq!(None, bitvec.get(0));
        assert_eq!(None, bitvec.get(1));

        bitvec.push(true);
        assert_eq!(Some(true), bitvec.get(0));
        assert_eq!(None, bitvec.get(1));

        bitvec.push(false);
        assert_eq!(Some(true), bitvec.get(0));
        assert_eq!(Some(false), bitvec.get(1));
        assert_eq!(None, bitvec.get(2));
    }

    #[test]
    fn bitvec_range() {
        let first = BitVec::from_slice(&[false, true, false]);
        assert_eq!(first, first.range(0..3).unwrap());
        assert_eq!(None, first.range(0..4));
    }

    #[test]
    fn bitvec_append() {
        let first = BitVec::from_slice(&[false, true, false]);
        let mut second = BitVec::new();
        second.push(true);
        second.append(&first);
        assert_eq!(first, second.range(1..4).unwrap());
        assert!(first != second.range(0..3).unwrap());
        assert!(first != second.range(1..3).unwrap());
    }

    #[test]
    fn compress() {
        let bits = BitVec::from_slice(&[true, true, false]);
        let vec = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let comp = super::compress(3, &vec).unwrap();
        let expected = BitVec::from_slice(&[true, true, true, false, false]);
        assert_eq!(expected, comp);
    }

    #[test]
    fn decompress() {
        let comp = BitVec::from_slice(&[true, true, true, false, false]);
        let bits = BitVec::from_slice(&[true, true, false]);
        let expected = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let decomp = super::decompress(3, &comp).unwrap();
        assert_eq!(expected, decomp);
    }

    #[test]
    fn round_trip() {
        let bits = BitVec::from_slice(&[true, true, false]);
        let expected = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let obj = CompressedBitVec::compress(&expected).unwrap();
        let decomp = obj.decompress().unwrap();
        assert_eq!(expected, decomp);
    }
}
