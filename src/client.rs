use controller_sequence as ctrl_seq;
use msg;
use std;
use bincode;
use serde;
use std::net::{TcpStream, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std::io::Read;

#[derive(Clone)]
pub enum ServicerMessage<StateT> {
    /// The latest segment of world state needed by a specific client.
    WorldState(StateT),
    /// A message that will be visible to all players.
    ChatMessage(msg::reliable::ChatMessage),
    /// Implies there is no message to send.
    None,
}

pub mod reliable {
    use super::*;

    struct Servicer {
        tcp_stream: TcpStream,
        to_application: mpsc::Sender<msg::reliable::ServerMessage>,
    }

    impl Servicer {
        // let mut tcp_stream = TcpStream::connect(&server_addr)?;
        // let (to_application, from_application) = mpsc::channel();
        pub fn new(
            self_addr: SocketAddrV4,
            server_addr: SocketAddrV4,
            tcp_stream: TcpStream,
            to_application: mpsc::Sender<msg::reliable::ServerMessage>,
        ) -> std::io::Result<Self> {
            Ok(Servicer {
                tcp_stream: tcp_stream,
                to_application: to_application,
            })
        }

        pub fn spin(&mut self) -> Result<(), msg::CommError> {
            // Receive ACK only from the connected server.
            let mut buf = Vec::<u8>::new();
            let bytes = self.tcp_stream.read(&mut buf).map_err(|err| {
                msg::CommError::from(err)
            })?;

            if bytes == 0 {
                // The Tcp stream has been closed.
                // Do more client management here.
                return Ok(());
            }

            // Deserialize the received datagram.
            // The ACK contains the latest controller input the server has received from us.
            let server_message: msg::reliable::ServerMessage = bincode::deserialize(&buf[..])
                .map_err(|err| {
                    msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                })?;

            match server_message {
                msg::reliable::ServerMessage::JoinResponse(response) => {
                    self.handle_join_response(response)
                }
                msg::reliable::ServerMessage::ChatMessage(chat) => self.handle_chat_message(chat),
                msg::reliable::ServerMessage::LastTickReceived(tick) => {
                    self.handle_last_tick_received(tick)
                }
                msg::reliable::ServerMessage::None => Ok(()),
            }
        }

        fn handle_last_tick_received(&mut self, last_tick: usize) -> Result<(), msg::CommError> {
            // TODO this needs to be handled in the application thread in order to sync with the
            // low latency servicer.
            // Now we remove the controller inputs the server has ACK'd.
            // self.controller_seq.remove_till_tick(last_tick + 1);
            self.to_application
                .send(msg::reliable::ServerMessage::LastTickReceived(last_tick))
                .map_err(|err| {
                    msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                })
        }

        fn handle_join_response(
            &self,
            response: msg::reliable::JoinResponse,
        ) -> Result<(), msg::CommError> {
            Ok(())
        }

        fn handle_chat_message(
            &mut self,
            chat: msg::reliable::ChatMessage,
        ) -> Result<(), msg::CommError> {
            self.to_application
                .send(msg::reliable::ServerMessage::ChatMessage(chat))
                .map_err(|err| {
                    msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                })
        }
    }
} // mod reliable

pub mod low_latency {
    use super::*;

    /// Services a client using a low-latency transport (Udp). Communicates messages to the
    /// application thread via an in-process queue.
    struct Servicer<StateT>
    where
        StateT: serde::Serialize,
        StateT: Send,
    {
        udp_socket: UdpSocket,
        server_addr: SocketAddrV4,
        self_addr: SocketAddrV4,
        controller_seq: ctrl_seq::ControllerSequence,
        to_application: mpsc::Sender<ServicerMessage<StateT>>,
    }

    impl<StateT> Servicer<StateT>
    where
        StateT: serde::Serialize,
        StateT: Send,
    {
        fn new(
            self_addr: SocketAddrV4,
            server_addr: SocketAddrV4,
            to_application: mpsc::Sender<ServicerMessage<StateT>>,
        ) -> std::io::Result<Self> {
            let mut udp_socket = UdpSocket::bind(&self_addr)?;
            udp_socket.connect(&server_addr)?;
            Ok(Servicer {
                udp_socket: udp_socket,
                server_addr: server_addr,
                self_addr: self_addr,
                controller_seq: ctrl_seq::ControllerSequence::new(),
                to_application: to_application,
            })
        }

        pub fn spin(&mut self) -> Result<(), msg::CommError>
        where
            StateT: serde::de::DeserializeOwned,
            StateT: serde::Serialize,
        {
            loop {
                // Receive ACK only from the connected server.
                let mut buf = Vec::<u8>::new();
                self.udp_socket.recv(&mut buf).map_err(|err| {
                    msg::CommError::from(err)
                })?;

                // Deserialize the received datagram.
                // The ACK contains the latest controller input the server has received from us.
                let server_message: msg::low_latency::ServerMessage<StateT> =
                    bincode::deserialize(&buf[..]).map_err(|err| {
                        msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                    })?;

                match server_message {
                    msg::low_latency::ServerMessage::WorldState(state) => {
                        self.handle_world_state(state)?;
                    }
                    msg::low_latency::ServerMessage::None => {}
                }
            }
        }

        fn handle_world_state<'de>(&mut self, state: StateT) -> Result<(), msg::CommError>
        where
            StateT: serde::Deserialize<'de>,
            StateT: serde::Serialize,
        {
            self.to_application
                .send(ServicerMessage::WorldState(state))
                .map_err(|err| {
                    msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                })
        }

        pub fn send_controller_inputs(&self) -> Result<(), msg::CommError> {
            if !self.controller_seq.is_empty() {
                let payload = self.controller_seq.to_compressed().ok_or(
                    msg::CommError::Warning(
                        msg::Warning::FailedToCompress,
                    ),
                )?;

                let controller_input = msg::low_latency::ClientMessage::ControllerInput(payload);

                let encoded: Vec<u8> = bincode::serialize(&controller_input).map_err(|err| {
                    msg::CommError::Warning(msg::Warning::FailedToSerialize(err))
                })?;

                self.udp_socket
                    .send_to(&encoded, self.server_addr)
                    .map_err(|err| msg::CommError::from(err))?;
            }

            Ok(())
        }
    }
} // mod low_latency
