use bincode;
use client;
use control;
use msg;
use serde;
use spmc;
use std::io::Read;
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std;

/// Services a client using a low-latency transport (Udp). Communicates messages to the
/// application thread via an in-process queue.
pub struct Servicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    receive_buf: Vec<u8>,
    udp_socket: UdpSocket,
    server_addr: SocketAddr,
    self_addr: SocketAddr,
    to_application: mpsc::Sender<msg::low_latency::ServerMessage<StateT>>,
    from_application: mpsc::Receiver<msg::low_latency::ClientMessage>,
}

impl<StateT> Servicer<StateT>
where
    StateT: serde::de::DeserializeOwned,
    StateT: serde::Serialize,
    StateT: Send,
{
    pub fn connect(
        mut udp_socket: UdpSocket,
        server_addr: SocketAddr,
        to_application: mpsc::Sender<client::ServicerMessage<StateT>>,
        from_application: mpsc::Receiver<msg::low_latency::ClientMessage>,
    ) -> std::io::Result<Self> {
        udp_socket.connect(&server_addr)?;
        udp_socket.set_nonblocking(true)?;
        Ok(Servicer {
            receive_buf: vec![],
            udp_socket: udp_socket,
            server_addr: server_addr,
            self_addr: udp_socket.local_addr()?,
            controller_seq: control::ControllerSequence::new(),
            to_application: to_application,
            // from_application: from_application,
        })
    }

    pub fn spin(&mut self) {
        loop {
            match self.spin_once() {
                Ok(()) => {}
                Err(msg::CommError::Warning(warn)) => {
                    warn!("Warning: {:?}", warn);
                }
                Err(msg::CommError::Drop(drop)) => {
                    warn!("Dropping: {:?}", drop);
                    break;
                }
                Err(msg::CommError::Exit) => {
                    break;
                }
            }
        }
        info!("Exiting client low latency servicer");
    }

    fn spin_once(&mut self) -> Result<(), msg::CommError> {}

    fn forward_from_server_to_client(&mut self) -> Result<(), msg::CommError> {
        loop {
            receive_buf.clear();
            match self.udp_socket.recv(&mut receive_buf) {
                Some(_) => {
                    // Deserialize the received datagram.
                    let server_message: msg::low_latency::ServerMessage<StateT> =
                        bincode::deserialize(&buf[..]).map_err(|err| {
                            msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                        })?;

                    // Send it on to the client application thread.
                    self.to_application.send(server_message).map_err(|err| {
                        msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                    })
                },
                Err(std::io::ErrorKind::WouldBlock) => {
                    // Assuming here that `WouldBlock` implies there is no data in the buffer.
                    return Ok(()),
                },
                Err(err) => {
                    return Err(msg::CommError::from(err));
                },
            };
        }
    }

    fn forward_from_client_to_server(&mut self) -> Result<(), msg::CommError> {
        loop {
            match self.from_application
        }
    }

    // TODO Do we want/need a from_application for this?
    fn send_controller_inputs(&self) -> Result<(), msg::CommError> {



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
