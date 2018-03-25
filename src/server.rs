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

/// Represents messages passed from client servicer threads to the main application thread.
#[derive(Clone)]
pub enum ServicerPayload {
    /// A message that will be visible to all players.
    ChatMessage(String),
    /// A series of control inputs -- one for each game tick since the last tick received.
    ControllerSequence(ctrl_seq::ControllerSequence),
    ///
    ClientData(msg::ClientData),
    /// Implies there is no message to send.
    None,
}

/// Represents messages passed from client servicer threads to the main application thread, along
/// with the user the message originated from over the network.
#[derive(Clone)]
pub struct ServicerMessage {
    /// Originator's user name.
    pub from: msg::Username,
    /// The message payload to be handled by the application thread.
    pub payload: ServicerPayload,
}

struct Server<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    low_latency_servicer: low_latency::Servicer<StateT>,
    reliable_join_handle: std::thread::JoinHandle<()>,
    from_servicer: mpsc::Receiver<ServicerMessage>,
    to_low_latency_servicer: spmc::Sender<low_latency::ApplicationMessage<StateT>>,
    to_reliable_servicer: spmc::Sender<msg::reliable::ServerMessage>,
}

impl<StateT> Server<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    pub fn new(udp_socket: UdpSocket, tcp_listener: TcpListener) -> Self {
        let (to_reliable_servicer, re_from_application) = spmc::channel();
        let (to_application, from_servicer) = mpsc::channel();
        let (to_low_latency_servicer, ll_from_application) = spmc::channel();
        let low_latency_servicer = low_latency::Servicer::<StateT>::new(
            udp_socket,
            to_application.clone(),
            ll_from_application,
        );

        let reliable_join_handle = std::thread::spawn(move || {
            reliable::Servicer::listen(tcp_listener, to_application, re_from_application);
        });

        Server {
            low_latency_servicer: low_latency_servicer,
            reliable_join_handle: reliable_join_handle,
            from_servicer: from_servicer,
            to_low_latency_servicer: to_low_latency_servicer,
            to_reliable_servicer: to_reliable_servicer,
        }
    }

    pub fn quit(mut self) -> std::thread::Result<()> {
        // Disconnect servicer message queues. This informs the servicers to disconnect from their
        // sockets and join their parent thread.
        drop(self.to_low_latency_servicer);
        drop(self.to_reliable_servicer);
        self.reliable_join_handle.join()
    }
}

pub mod low_latency {
    use super::*;

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
        to_application: mpsc::Sender<ServicerMessage>,

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
            to_application: mpsc::Sender<ServicerMessage>,
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

        fn process_udp(
            &mut self,
            src: std::net::SocketAddr,
            buf: &[u8],
        ) -> Result<(), msg::CommError> {
            // Deserialize the received datagram.
            let client_message: msg::low_latency::ClientMessage = bincode::deserialize(&buf)
                .map_err(|err| {
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
        ) -> Result<ServicerMessage, msg::CommError> {
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

            Ok(ServicerMessage {
                from: user.clone(),
                payload: ServicerPayload::ControllerSequence(controller_seq),
            })
        }
    }
}

pub mod reliable {
    use super::*;

    /// Services a single client using a reliable transport (Tcp). Communicates messages to the
    /// application thread via an in-process queue.
    pub struct Servicer {
        /// Our reliable transport to/from the client.
        tcp_stream: TcpStream,

        /// Our in-process transport to the application thread.
        to_application: mpsc::Sender<ServicerMessage>,

        /// Our in-process transport from the application thread to all reliable servicers.
        from_application: spmc::Receiver<msg::reliable::ServerMessage>,

        /// The user this servicer is servicing.
        user: msg::Username,

        /// When `true`, the server will terminate any open connections and stop servicing inputs.
        shutting_down: bool,
    }

    impl Servicer {
        /// Upon new incoming connection, create a reliable servicer:
        /// - Clone to_application.
        /// - Clone reliable from_application.
        /// From the reliable servicer:
        /// - Authenticate the new connection.
        /// - Establish a client-side UDP socket.
        /// - Begin servicing game messages.
        /// Once the listener errors out, attempt to join each servicer.
        pub fn listen(
            tcp_listener: TcpListener,
            to_application: mpsc::Sender<ServicerMessage>,
            from_application: spmc::Receiver<msg::reliable::ServerMessage>,
        ) {
            let mut servicer_threads = Vec::new();
            for stream in tcp_listener.incoming() {
                match stream {
                    Err(err) => {
                        comm_log!(msg::CommError::Drop(msg::Drop::IoFailure(err)));
                    }
                    Ok(stream) => {
                        let servicer_to_application = to_application.clone();
                        let servicer_from_application = from_application.clone();
                        let servicer_thread = std::thread::spawn(move || match Servicer::new(
                            stream,
                            servicer_to_application,
                            servicer_from_application,
                        ) {
                            Ok(mut servicer) => {
                                servicer.spin();
                            }
                            Err(err) => {
                                log_and_exit!(err);
                            }
                        });
                        servicer_threads.push(servicer_thread);
                    }
                }
            }
            for servicer_thread in servicer_threads.drain(..) {
                servicer_thread.join().unwrap();
            }
        }

        pub fn new(
            mut tcp_stream: TcpStream,
            to_application: mpsc::Sender<ServicerMessage>,
            from_application: spmc::Receiver<msg::reliable::ServerMessage>,
        ) -> Result<Self, msg::CommError> {
            let cred = Servicer::wait_for_join_request(&mut tcp_stream)?;

            // Authenticate the user.
            let mut client_data = Servicer::authenticate_user(cred)?;

            // Make the servicer.
            let mut servicer = Servicer {
                tcp_stream: tcp_stream,
                to_application: to_application,
                from_application: from_application,
                user: client_data.username.clone(),
                shutting_down: false,
            };

            // Send client_data to the application thread.
            let client_data_msg = ServicerMessage {
                from: client_data.username.clone(),
                payload: ServicerPayload::ClientData(client_data),
            };
            servicer.to_application.send(client_data_msg).map_err(|_| {
                msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
            })?;

            Ok(servicer)
        }

        pub fn spin(&mut self) {
            loop {
                match self.spin_once() {
                    Ok(()) => {}
                    Err(err) => {
                        match err {
                            msg::CommError::Warning(warn) => {
                                warn!("Warning {:?}: {:?}", self.user, warn);
                            }
                            msg::CommError::Drop(drop) => {
                                error!("Dropping {:?}: {:?}", self.user, drop);
                                self.shutdown();
                                return;
                            }
                        }
                    }
                }
            }
        }

        fn spin_once(&mut self) -> Result<(), msg::CommError> {
            // Receive inputs from anyone.
            let mut buf = Vec::<u8>::new();
            self.tcp_stream.read(&mut buf).map_err(|err| {
                msg::CommError::from(err)
            })?;

            // Deserialize the received datagram.
            let client_message: msg::reliable::ClientMessage =
                bincode::deserialize(&buf).map_err(|err| {
                    msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                })?;

            let response = match client_message {
                msg::reliable::ClientMessage::ChatMessage(msg) => self.handle_chat_message(msg),
                msg::reliable::ClientMessage::JoinRequest(cred) => Err(msg::CommError::Drop(
                    msg::Drop::AlreadyConnected,
                )),
                msg::reliable::ClientMessage::None => Ok(msg::reliable::ServerMessage::None),
            }?;

            // Serialize the tick.
            let encoded_response = bincode::serialize(&response).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToSerializeAck(err))
            })?;

            // Send the tick.
            self.tcp_stream.write_all(&encoded_response).map_err(
                |err| {
                    msg::CommError::from(err)
                },
            )?;
            Ok(())
        }

        fn poll_application_thread(&mut self) -> Vec<msg::reliable::ServerMessage> {
            let mut msg_vec = vec![];
            loop {
                // Receive inputs from the application thread.
                match self.from_application.try_recv() {
                    Ok(msg) => {
                        msg_vec.push(msg);
                    }
                    Err(spmc::TryRecvError::Empty) => {
                        return msg_vec;
                    }
                    Err(spmc::TryRecvError::Disconnected) => {
                        // The application thread disconnected; we should begin shutting down.
                        self.shutdown();
                        return msg_vec;
                    }
                }
            }
        }

        fn shutdown(&mut self) {
            self.shutting_down = true;
        }

        fn expect_join_request(buf: &Vec<u8>) -> Result<msg::Credentials, msg::CommError> {
            let client_message: msg::reliable::ClientMessage =
                bincode::deserialize(&buf).map_err(|err| {
                    msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
                })?;

            match client_message {
                msg::reliable::ClientMessage::JoinRequest(cred) => Ok(cred),
                _ => Err(msg::CommError::Warning(msg::Warning::WrongMessageType)),
            }
        }

        fn wait_for_join_request(
            tcp_stream: &mut TcpStream,
        ) -> Result<msg::Credentials, msg::CommError> {
            loop {
                let mut buf = Vec::<u8>::new();
                match tcp_stream.read(&mut buf) {
                    Ok(_) => {
                        return Servicer::expect_join_request(&mut buf);
                    }
                    Err(err) => {
                        match msg::CommError::from(err) {
                            msg::CommError::Drop(drop) => {
                                return Err(msg::CommError::Drop(drop));
                            }
                            msg::CommError::Warning(warn) => {
                                comm_log!(msg::CommError::Warning(warn));
                            }
                        }
                    }
                };
            }
        }

        fn authenticate_user(cred: msg::Credentials) -> Result<msg::ClientData, msg::CommError> {
            // TODO
            Ok(msg::ClientData {
                username: cred.username,
                udp_addr: cred.udp_addr,
            })
        }

        fn handle_chat_message(
            &self,
            msg: String,
        ) -> Result<msg::reliable::ServerMessage, msg::CommError> {
            // Send the chat message back to the application thread for broadcast.
            self.to_application
                .send(ServicerMessage {
                    from: self.user.clone(),
                    payload: ServicerPayload::ChatMessage(msg),
                })
                .map_err(|_| {
                    msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
                })?;

            // No feedback necessary.
            Ok(msg::reliable::ServerMessage::None)
        }
    }
}
