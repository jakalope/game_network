use bincode;
use client;
use control;
use msg;
use serde;
use std::io::Read;
use std::net::{TcpStream, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std;

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
    controller_seq: control::ControllerSequence,
    to_application: mpsc::Sender<client::ServicerMessage<StateT>>,
}

impl<StateT> Servicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    fn new(
        self_addr: SocketAddrV4,
        server_addr: SocketAddrV4,
        to_application: mpsc::Sender<client::ServicerMessage<StateT>>,
    ) -> std::io::Result<Self> {
        let mut udp_socket = UdpSocket::bind(&self_addr)?;
        udp_socket.connect(&server_addr)?;
        Ok(Servicer {
            udp_socket: udp_socket,
            server_addr: server_addr,
            self_addr: self_addr,
            controller_seq: control::ControllerSequence::new(),
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
            .send(client::ServicerMessage::WorldState(state))
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
