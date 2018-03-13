extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate bincode;
extern crate bidir_map;

mod bitvec;
mod controller_sequence;

use std::sync::mpsc;
use std::net::{SocketAddr, UdpSocket};
use std::collections::HashMap;
use std::collections::hash_map::Entry;

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug, Hash, Eq)]
pub struct Username(usize);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, Eq)]
pub struct Credentials {
    pub username: Username,
    pub password: String,
}

pub enum SendError {
    FailedToCompress,
    FailedToSerialize(Box<bincode::ErrorKind>),
    FailedToSend(std::io::Error),
}

pub enum RecvError {
    UnknownSource(SocketAddr),
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
    Confirmation,
    AuthenticationError,
}

/// Represents all possible message types a `Client` can send a `Server`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// A message to send to the server that will be visible to all players.
    // ChatMessage(String),
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
    // ChatMessage { username: String, message: String },
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
    client_inputs: HashMap<Username, controller_sequence::ControllerSequence>,
    address_user: bidir_map::BidirMap<SocketAddr, Username>,
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
            // ServerMessage::ChatMessage {
            //     username: user,
            //     message: msg,
            // } => self.handle_chat_message(user, msg),
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
        src: SocketAddr,
        input: controller_sequence::CompressedControllerSequence,
    ) -> Result<ServerMessage<StateT>, RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        // Lookup the username given the source socket address.
        let user = self.address_user.get_by_first(&src).ok_or(
            RecvError::UnknownSource(src),
        )?;

        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            RecvError::FailedToDecompress,
        )?;

        // Compute game tick of last controller input received.
        let last_tick: ServerMessage<StateT> =
            ServerMessage::LastTickReceived(controller_seq.last_tick());

        // Append the inputs in the client inputs map.
        match self.client_inputs.entry(*user) {
            Entry::Occupied(entry) => {
                entry.into_mut().append(controller_seq);
            }
            Entry::Vacant(entry) => {
                entry.insert(controller_seq);
            }
        }

        // Return the appropriate ACK.
        Ok(last_tick)
    }

    fn handle_join_request<StateT>(
        &mut self,
        src: SocketAddr,
        cred: Credentials,
    ) -> Result<ServerMessage<StateT>, RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        // Authenticate the user before proceeding.
        if !self.authenticate_user(&cred) {
            // From the server's perspective, this isn't an error. It's request that deserves a
            // negative response.
            return Ok(ServerMessage::JoinResponse(
                JoinResponse::AuthenticationError,
            ));
        }

        let mut disconnect_user: Option<Username> = None;
        if let Some(user) = self.address_user.get_by_first(&src) {
            // The source address is already associated with a user.
            if *user == cred.username {
                // The user is already connected. Just send back a confirmation. This might happen
                // if the user hasn't received their confirmation from a previous attempt.
                return Ok(ServerMessage::JoinResponse(JoinResponse::Confirmation));
            } else {
                // The source address holder seems to be changing their username but hasn't
                // disconnected first. Boot `user` and reconnect.
                disconnect_user = Some(*user);
            }
        }

        if let Some(user) = disconnect_user {
            self.disconnect_user(&user);
        }

        self.connect_user(&src, &cred.username);
        Ok(ServerMessage::JoinResponse(JoinResponse::Confirmation))
    }

    fn connect_user(&mut self, src: &SocketAddr, user: &Username) {
        self.address_user.insert(*src, *user);
    }

    fn disconnect_user(&mut self, user: &Username) {
        self.address_user.remove_by_second(user);
    }

    fn authenticate_user(&self, cred: &Credentials) -> bool {
        // TODO
        true
    }

    //     fn handle_chat_message<StateT>(
    //         &mut self,
    //         src: SocketAddr,
    //         msg: String,
    //     ) -> Result<ServerMessage<StateT>, RecvError>
    //     where
    //         StateT: serde::Deserialize<'de>,
    //         StateT: serde::Serialize,
    //     {
    //         // TODO Chat should be handled over TCP.
    //         Ok(())
    //     }

    // fn broadcast_chat_message<StateT>(
    //     &self,
    //     src: SocketAddr,
    //     msg: String,
    // ) -> Result<ServerMessage<StateT>, RecvError> {
    //     let Ok(user) = self.address_user.get_by_first(&src).ok_or(
    //         RecvError::UnknownSource(src),
    //     )?;
    //     let mut displayed_message = user.clone();
    //     displayed_message.push_str(": ");
    //     displayed_message.push_str(msg);
    //     Ok(ServerMessage::ChatMessage(displayed_message))
    // }

    pub fn broadcast_world_state<StateT>(&self, state: StateT) -> Result<(), SendError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        let encode = bincode::serialize(&state).map_err(|err| {
            SendError::FailedToSerialize(err)
        })?;

        for address_user in self.address_user.iter() {
            self.socket.send_to(&encode, address_user.0);
        }

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

        let response = match client_message {
            ClientMessage::ControllerInput(input) => self.handle_controller::<StateT>(src, input),
            ClientMessage::JoinRequest(cred) => self.handle_join_request::<StateT>(src, cred),
            // ClientMessage::ChatMessage(msg) => self.handle_chat_message::<StateT>(src, msg),
        }?;

        // Serialize the tick.
        let encode = bincode::serialize(&response).map_err(|err| {
            RecvError::FailedToSerializeAck(err)
        })?;

        // Send the tick.
        self.socket.send_to(&encode, src).map_err(|err| {
            RecvError::FailedToSendAck(err)
        })?;

        Ok(())
    }
}
