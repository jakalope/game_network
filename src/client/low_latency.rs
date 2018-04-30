use bincode;
use bitvec;
use client;
use control;
use msg;
use serde;
use spmc;
use std::io::Read;
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std;
use drain_mpsc_receiver;

/// Represents messages a client application thread can send to a client low latency servicer
/// thread.
#[derive(Clone)]
pub enum ApplicationMessage {
    /// An uncompressed bit-vector, representing a snapshot of the player's controls.
    Control(bitvec::BitVec),
}

/// Represents messages a client low latency servicer thread can send to a client application
/// thread.
#[derive(Clone)]
pub enum ServicerMessage {
    WorldState(Vec<u8>),
}

/// Services a client using a low-latency transport (Udp). Communicates messages to the
/// application thread via an in-process queue.
pub struct Servicer {
    receive_buf: Vec<u8>,
    udp_socket: UdpSocket,
    server_addr: SocketAddr,
    self_addr: SocketAddr,
    controller_seq: control::ControllerSequence,
    to_application: mpsc::Sender<ServicerMessage>,
    from_application: mpsc::Receiver<ApplicationMessage>,
}

impl Servicer {
    pub fn connect(
        udp_socket: UdpSocket,
        server_addr: SocketAddr,
        to_application: mpsc::Sender<ServicerMessage>,
        from_application: mpsc::Receiver<ApplicationMessage>,
    ) -> std::io::Result<Self> {
        udp_socket.connect(&server_addr)?;
        udp_socket.set_nonblocking(true)?;
        let local_addr = udp_socket.local_addr()?;
        Ok(Servicer {
            receive_buf: vec![],
            udp_socket: udp_socket,
            server_addr: server_addr,
            self_addr: local_addr,
            controller_seq: control::ControllerSequence::new(),
            to_application: to_application,
            from_application: from_application,
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

    fn spin_once(&mut self) -> Result<(), msg::CommError> {
        self.forward_from_server_to_client()?;
        self.forward_from_client_to_server()?;
        Ok(())
    }

    fn forward_from_server_to_client(&mut self) -> Result<(), msg::CommError> {
        loop {
            self.receive_buf.clear();
            match self.udp_socket.recv(&mut self.receive_buf) {
                Ok(_) => {
                    // Deserialize the received datagram.
                    let server_message: msg::low_latency::ServerMessage =
                        bincode::deserialize(&self.receive_buf[..]).map_err(|err| {
                            msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                        })?;
                    return self.parse_server_message(server_message);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // Assuming here that `WouldBlock` implies there is no data in the buffer.
                    return Ok(());
                }
                Err(err) => {
                    return Err(msg::CommError::from(err));
                }
            };
        }
    }

    fn parse_server_message(
        &mut self,
        server_message: msg::low_latency::ServerMessage,
    ) -> Result<(), msg::CommError> {
        match server_message {
            msg::low_latency::ServerMessage::WorldState(state) => {
                // Send it on to the client application thread.
                return self.to_application
                    .send(ServicerMessage::WorldState(state))
                    .map_err(|err| {
                        msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                    });
            }
            msg::low_latency::ServerMessage::LastTickReceived(tick) => {
                // Update the controller sequence, removing inputs of ticks already received by the
                // server.
                self.controller_seq.remove_till_tick(tick);
                return Ok(());
            }
        }
    }

    fn forward_from_client_to_server(&mut self) -> Result<(), msg::CommError> {
        match drain_mpsc_receiver(&mut self.from_application) {
            Ok(mut msg_vec) => {
                self.parse_application_messages(&mut msg_vec);
                return self.send_controller_inputs();
            }
            Err(_) => {
                return Err(msg::CommError::Exit);
            }
        }
    }

    // Parse a vector of app messages.
    fn parse_application_messages(&mut self, msg_vec: &mut Vec<ApplicationMessage>) {
        for msg in msg_vec.drain(..) {
            match msg {
                ApplicationMessage::Control(controller_state) => {
                    self.controller_seq.push(controller_state);
                }
            }
        }
    }

    // Send controller inputs from the client to the server over this low latency transport.
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
