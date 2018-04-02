use server;
use server::reliable;
use msg;
use controller_sequence as ctrl_seq;
use std;
use std::net::{TcpStream, TcpListener, UdpSocket};
use std::sync::mpsc;
use std::io::{Write, Read};
use serde;
use bincode;
use spmc;
use bidir_map;


/// Represents a message from the application thread to the low latency servicer, bound for the
/// client.
#[derive(Clone)]
pub struct ToClient<StateT>
where
    StateT: serde::Serialize,
{
    /// The user `payload` is intended for.
    pub to: msg::Username,

    /// The payload intended for the user `to`.
    pub payload: msg::low_latency::ServerMessage<StateT>,
}

/// Represents a message from the application thread to the low latency servicer.
#[derive(Clone)]
pub enum ApplicationMessage<StateT>
where
    StateT: serde::Serialize,
{
    /// A message to be forwarded to a specific client.
    ToClient(ToClient<StateT>),

    /// When a new client connects, the low latency servicer updates its user/socket map.
    NewClient(msg::ClientData),

    /// When a client disconnects, the low latency servicer updates its user/socket map.
    ClientDisconnect(msg::Username),
}

/// Services all clients using a low-latency transport (Udp). Communicates messages to the
/// application thread via an in-process queue.
pub struct Servicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    /// Our low-latency transport from the client.
    udp_socket: UdpSocket,

    /// Our in-process transport to the application thread.
    to_application: mpsc::Sender<server::ServicerMessage>,

    /// Our in-process transport from the application thread to all low-latency servicers.
    // TODO Do we want more than one Udp socket to send things like world-state concurrently?
    // We could easily use this queue as a job queue and generate N low latency senders.
    from_application: spmc::Receiver<ApplicationMessage<StateT>>,

    address_user: bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,

    /// When `true`, the server will terminate any open connections and stop servicing inputs.
    shutting_down: bool,
}

impl<'de, StateT> Servicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    pub fn new(
        udp_socket: UdpSocket,
        to_application: mpsc::Sender<server::ServicerMessage>,
        from_application: spmc::Receiver<ApplicationMessage<StateT>>,
    ) -> Self {
        Servicer {
            udp_socket: udp_socket,
            to_application: to_application,
            from_application: from_application,
            address_user: bidir_map::BidirMap::<std::net::SocketAddr, msg::Username>::new(),
            shutting_down: false,
        }
    }

    fn poll_application_thread(&mut self) -> Vec<ToClient<StateT>> {
        let mut msg_vec = vec![];
        loop {
            // Receive inputs from the application thread.
            match self.from_application.try_recv() {
                Ok(msg) => {
                    match msg {
                        ApplicationMessage::ToClient(to_client) => {
                            msg_vec.push(to_client);
                        }
                        ApplicationMessage::NewClient(client_data) => {
                            self.address_user.insert(
                                std::net::SocketAddr::V4(client_data.udp_addr),
                                client_data.username,
                            );
                        }
                        ApplicationMessage::ClientDisconnect(user) => {
                            self.address_user.remove_by_second(&user);
                        }
                    }
                }
                Err(spmc::TryRecvError::Empty) => {
                    return msg_vec;
                }
                Err(spmc::TryRecvError::Disconnected) => {
                    // The application thread disconnected; we should begin shutting down.
                    self.shutdown();
                }
            } // match
        } // loop
    } // fn poll_application_thread()

    pub fn spin(&mut self) {
        // Initialize.
        if let Err(err) = self.udp_socket.set_nonblocking(true) {
            error!("{:?}", err);
            self.shutdown();
            return;
        }
        // Loop till we shutdown.
        loop {
            match self.spin_once() {
                Ok(()) => {}
                Err(msg::CommError::Drop(drop)) => {
                    error!("{:?}", drop);
                    self.shutdown();
                    return;
                }
                Err(msg::CommError::Warning(warn)) => {
                    warn!("{:?}", warn);
                }
            }
        }
    }

    fn spin_once(&mut self) -> Result<(), msg::CommError> {
        let mut msg_vec = self.poll_application_thread();
        for msg in msg_vec.drain(..) {
            let encode = bincode::serialize(&msg.payload).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToSerialize(err))
            })?;

            // If we have the designated user in our address book, send them the
            // message via Udp. Otherwise, log error.
            if let Some(address) = self.address_user.get_by_second(&msg.to) {
                self.udp_socket.send_to(&encode, address).map_err(|err| {
                    msg::CommError::Warning(msg::Warning::IoFailure(err))
                })?;
            } else {
                warn!("No such user: {:?}", msg.to);
            }
        }

        // Receive inputs from the network.
        let mut buf = Vec::<u8>::new();
        match self.udp_socket.recv_from(&mut buf) {
            Ok((_, src)) => self.process_udp(src, &buf),
            Err(err) => Err(msg::CommError::Warning(msg::Warning::IoFailure(err))),
        } // match
    }

    fn process_udp(&mut self, src: std::net::SocketAddr, buf: &[u8]) -> Result<(), msg::CommError> {
        // Deserialize the received datagram.
        let client_message: msg::low_latency::ClientMessage =
            bincode::deserialize(&buf).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
            })?;

        let response = match client_message {
            msg::low_latency::ClientMessage::ControllerInput(input) => {
                self.handle_controller(src, input)
            }
            msg::low_latency::ClientMessage::None => {
                return Ok(());
            }
        }?;

        if let Err(_) = self.to_application.send(response) {
            // Application thread closed the in-process connection, so we should exit
            // gracefully.
            self.shutdown();
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn handle_controller(
        &mut self,
        src: std::net::SocketAddr,
        input: ctrl_seq::CompressedControllerSequence,
    ) -> Result<server::ServicerMessage, msg::CommError> {
        // Lookup the username given the source socket address.
        let user = self.address_user.get_by_first(&src).ok_or(
            msg::CommError::Drop(
                msg::Drop::UnknownSource(src),
            ),
        )?;

        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            msg::CommError::Warning(
                msg::Warning::FailedToDecompress,
            ),
        )?;

        Ok(server::ServicerMessage {
            from: user.clone(),
            payload: server::ServicerPayload::ControllerSequence(controller_seq),
        })
    }
}
