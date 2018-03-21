extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate bincode;
extern crate bidir_map;
extern crate spmc;

mod bitvec;
mod controller_sequence;
mod msg;

use controller_sequence as ctrl_seq;
use std::sync::Arc;
use std::net::{TcpStream, TcpListener, Ipv4Addr, SocketAddrV4, UdpSocket};
use std::collections::HashMap;
use std::sync::mpsc;
use std::io::{Read, Write};

pub struct Server {
    /// Low-latency transport between server and clients.
    udp_socket: UdpSocket,

    /// Shared hash map with thread safe client data.
    /// The hash map's keys should be immutable after construction. The values must be mutable by
    /// one thread at a time.
    client: Arc<HashMap<msg::Username, msg::ClientData>>,

    /// When a Udp packet comes in, use this to associate it with a particular user.
    udp_to_user: HashMap<SocketAddrV4, msg::Username>,
}

impl<'de> Server {
    fn new() -> std::io::Result<Self> {
        // Grab a random Udp socket.
        let loopback = Ipv4Addr::new(127, 0, 0, 1);
        let self_addr = SocketAddrV4::new(loopback, 0);
        let udp_socket = UdpSocket::bind(self_addr)?;
        Ok(Server {
            udp_socket: udp_socket,
            client: Arc::new(HashMap::new()),
            udp_to_user: HashMap::new(),
        })
    }

    /// Assumes authentication is complete.
    // fn add_client(&mut self, tcp_stream: TcpStream) -> std::io::Result<()> {
    //     match self.client.entry(username) {
    //         Entry::Occupied(entry) => {
    //             entry.into_mut().inputs.append(controller_seq);
    //         }
    //         Entry::Vacant(entry) => {
    //             entry.inputs.insert(controller_seq);
    //         }
    //     }

    //     self.client.insert(
    //         username.clone(),
    //         msg::ClientData {
    //             tcp_stream: tcp_stream,
    //             username: username,
    //             udp_addr: SocketAddrV4,
    //             controller_seq: ctrl_seq::ControllerSequence,
    //         },
    //     );

    //     Ok(())
    // }

    // TODO we need a way to filter the state down to only what is needed for each client.
    pub fn broadcast_world_state<StateT>(&self, state: StateT) -> Result<(), msg::CommError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        let encode = bincode::serialize(&state).map_err(|err| {
            msg::CommError::FailedToSerialize(err)
        })?;

        for address_user in self.udp_to_user.iter() {
            // TODO Handle failure to send here instead of deferring to the caller.
            self.udp_socket.send_to(&encode, address_user.0).map_err(
                |err| {
                    msg::CommError::FailedToSend(err)
                },
            )?;
        }

        Ok(())
    }
}

pub struct Client<StateT> {
    tcp_stream: TcpStream,
    udp_socket: UdpSocket,
    server_addr: SocketAddrV4,
    self_addr: SocketAddrV4,
    controller_seq: ctrl_seq::ControllerSequence,
    to_application: mpsc::Sender<msg::ClientInProcMessage<StateT>>,
}

impl<StateT> Client<StateT> {
    fn new(
        self_addr: SocketAddrV4,
        server_addr: SocketAddrV4,
    ) -> std::io::Result<(Self, mpsc::Receiver<msg::ClientInProcMessage<StateT>>)> {
        let mut tcp_stream = TcpStream::connect(&server_addr)?;
        let mut udp_socket = UdpSocket::bind(&self_addr)?;
        udp_socket.connect(&server_addr)?;
        let (to_application, from_application) = mpsc::channel();
        Ok((
            Client {
                tcp_stream: tcp_stream,
                udp_socket: udp_socket,
                server_addr: server_addr,
                self_addr: self_addr,
                controller_seq: ctrl_seq::ControllerSequence::new(),
                to_application: to_application,
            },
            from_application,
        ))
    }

    pub fn send_controller_inputs(&self) -> Result<(), msg::CommError> {
        if !self.controller_seq.is_empty() {
            let payload = self.controller_seq.to_compressed().ok_or(
                msg::CommError::FailedToCompress,
            )?;

            let controller_input = msg::ClientLowLatencyMessage::ControllerInput(payload);

            let encoded: Vec<u8> = bincode::serialize(&controller_input).map_err(|err| {
                msg::CommError::FailedToSerialize(err)
            })?;

            self.udp_socket
                .send_to(&encoded, self.server_addr)
                .map_err(|err| msg::CommError::FailedToSend(err))?;
        }

        Ok(())
    }

    fn handle_last_tick_received(&mut self, last_tick: usize) -> Result<(), msg::CommError> {
        // Now we remove the controller inputs the server has ACK'd.
        self.controller_seq.remove_till_tick(last_tick + 1);
        Ok(())
    }

    fn handle_join_response(&self, response: msg::JoinResponse) -> Result<(), msg::CommError> {
        Ok(())
    }

    fn handle_world_state<'de>(&mut self, state: StateT) -> Result<(), msg::CommError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        self.to_application
            .send(msg::ClientInProcMessage::WorldState(state))
            .map_err(|err| msg::CommError::ApplicationThreadDisconnected)
    }

    fn handle_chat_message(&mut self, chat: msg::ChatMessage) -> Result<(), msg::CommError> {
        self.to_application
            .send(msg::ClientInProcMessage::ChatMessage(chat))
            .map_err(|err| msg::CommError::ApplicationThreadDisconnected)
    }

    pub fn receive_low_latency(&mut self) -> Result<(), msg::CommError>
    where
        StateT: serde::de::DeserializeOwned,
        StateT: serde::Serialize,
    {
        // Receive ACK only from the connected server.
        let mut buf = Vec::<u8>::new();
        self.udp_socket.recv(&mut buf).map_err(|err| {
            msg::CommError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        // The ACK contains the latest controller input the server has received from us.
        let server_message: msg::ServerLowLatencyMessage<StateT> =
            bincode::deserialize(&buf[..]).map_err(|err| {
                msg::CommError::FailedToDeserialize(err)
            })?;

        match server_message {
            msg::ServerLowLatencyMessage::WorldState(state) => self.handle_world_state(state),
            msg::ServerLowLatencyMessage::None => Ok(()),
        }
    }

    pub fn receive_reliable(&mut self) -> Result<(), msg::CommError> {
        // Receive ACK only from the connected server.
        let mut buf = Vec::<u8>::new();
        let bytes = self.tcp_stream.read(&mut buf).map_err(|err| {
            msg::CommError::FailedToReceive(err)
        })?;

        if bytes == 0 {
            // The Tcp stream has been closed.
            // Do more client management here.
            return Ok(());
        }

        // Deserialize the received datagram.
        // The ACK contains the latest controller input the server has received from us.
        let server_message: msg::ServerReliableMessage =
            bincode::deserialize(&buf[..]).map_err(|err| {
                msg::CommError::FailedToDeserialize(err)
            })?;

        match server_message {
            msg::ServerReliableMessage::JoinResponse(response) => {
                self.handle_join_response(response)
            }
            msg::ServerReliableMessage::ChatMessage(chat) => self.handle_chat_message(chat),
            msg::ServerReliableMessage::LastTickReceived(tick) => {
                self.handle_last_tick_received(tick)
            }
            msg::ServerReliableMessage::None => Ok(()),
        }
    }
}

/// Services all clients using a low-latency transport (Udp). Communicates messages to the
/// application thread via an in-process queue.
struct ClientLowLatencyServicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    /// Our low-latency transport from the client.
    udp_socket: UdpSocket,

    /// Our in-process transport to the application thread.
    to_application: mpsc::Sender<msg::ServicerMessage>,

    /// Our in-process transport from the application thread to all low-latency servicers.
    // TODO Do we want more than one Udp socket to send things like world-state concurrently?
    // We could easily use this queue as a job queue and generate N low latency senders.
    from_application: spmc::Receiver<msg::ApplicationMessage<StateT>>,

    address_user: bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,
}

enum AppThreadResult<StateT>
where
    StateT: serde::Serialize,
{
    MessageVec(Vec<msg::ToClient<StateT>>),
    BeginShutdown,
}

impl<'de, StateT> ClientLowLatencyServicer<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    fn poll_application_thread(&mut self) -> AppThreadResult<StateT> {
        let mut msg_vec = vec![];
        loop {
            // Receive inputs from the application thread.
            match self.from_application.try_recv() {
                Ok(msg) => {
                    match msg {
                        msg::ApplicationMessage::ToClient(to_client) => {
                            msg_vec.push(to_client);
                        }
                        msg::ApplicationMessage::NewClient(client_data) => {
                            self.address_user.insert(
                                std::net::SocketAddr::V4(client_data.udp_addr),
                                client_data.username,
                            );
                        }
                        msg::ApplicationMessage::ClientDisconnect(user) => {
                            self.address_user.remove_by_second(&user);
                        }
                    }
                }
                Err(spmc::TryRecvError::Empty) => {
                    return AppThreadResult::MessageVec(msg_vec);
                }
                Err(spmc::TryRecvError::Disconnected) => {
                    // The application thread disconnected; we should begin shutting down.
                    return AppThreadResult::BeginShutdown;
                }
            }
        }
    }

    pub fn spin(&mut self) -> Result<(), msg::CommError> {
        loop {
            match self.poll_application_thread() {
                AppThreadResult::BeginShutdown => {
                    return Ok(());
                }
                AppThreadResult::MessageVec(mut msg_vec) => {
                    for msg in msg_vec.drain(..) {
                        let encode = bincode::serialize(&msg.payload).map_err(|err| {
                            msg::CommError::FailedToSerialize(err)
                        })?;

                        // If we have the designated user in our address book, send them the
                        // message via Udp.
                        if let Some(address) = self.address_user.get_by_second(&msg.to) {
                            self.udp_socket.send_to(&encode, address).map_err(|err| {
                                msg::CommError::FailedToSend(err)
                            })?;
                        }
                    }
                }
            }
            // Receive inputs from the network.
            let mut buf = Vec::<u8>::new();
            let (_, src) = self.udp_socket.recv_from(&mut buf).map_err(|err| {
                msg::CommError::FailedToReceive(err)
            })?;

            // Deserialize the received datagram.
            let client_message: msg::ClientLowLatencyMessage =
                bincode::deserialize(&buf).map_err(|err| {
                    msg::CommError::FailedToDeserialize(err)
                })?;

            let response = match client_message {
                msg::ClientLowLatencyMessage::ControllerInput(input) => {
                    self.handle_controller(src, input)
                }
                msg::ClientLowLatencyMessage::None => {
                    continue;
                }
            }?;

            if let Err(_) = self.to_application.send(response) {
                // Application thread closed the in-process connection, so we should exit
                // gracefully.
                return Ok(());
            }
        }
    }

    fn handle_controller(
        &mut self,
        src: std::net::SocketAddr,
        input: ctrl_seq::CompressedControllerSequence,
    ) -> Result<msg::ServicerMessage, msg::CommError> {
        // Lookup the username given the source socket address.
        let user = self.address_user.get_by_first(&src).ok_or(
            msg::CommError::UnknownSource(src),
        )?;

        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            msg::CommError::FailedToDecompress,
        )?;

        Ok(msg::ServicerMessage {
            from: user.clone(),
            payload: msg::ServicerPayload::ControllerSequence(controller_seq),
        })

        // TODO The following belongs in the application thread.
        // // Compute game tick of last controller input received.
        // let last_tick: msg::ServerReliableMessage =
        //     msg::ServerLowLatencyMessage::LastTickReceived(controller_seq.last_tick());

        // Append the inputs in the client inputs map.
        // match self.client.entry(*user) {
        //     Entry::Occupied(entry) => {
        //         entry.into_mut().inputs.append(controller_seq);
        //     }
        //     Entry::Vacant(entry) => {
        //         entry.inputs.insert(controller_seq);
        //     }
        // }

        // Return the appropriate ACK.
        // Ok(last_tick)
    }
}

/// Services a single client using a reliable transport (Tcp). Communicates messages to the
/// application thread via an in-process queue.
struct ClientReliableServicer {
    /// Our reliable transport to/from the client.
    tcp_stream: TcpStream,

    /// Our in-process transport to the application thread.
    to_application: mpsc::Sender<msg::ServicerMessage>,

    /// Our in-process transport from the application thread to all reliable servicers.
    from_application: spmc::Receiver<msg::ServerReliableMessage>,

    user: msg::Username,
}

impl ClientReliableServicer {
    // TODO put these in the server.
    // let (to_servicer, from_application) = spmc::<msg::ServerReliableMessage>::channel();
    // let (to_application, from_servicer) = mpsc::<msg::ServicerMessage>::channel();
    /// Upon new incoming connection, create a reliable servicer:
    /// - Clone to_application.
    /// - Clone reliable from_application.
    /// From the reliable servicer:
    /// - Authenticate the new connection.
    /// - Establish a client-side UDP socket.
    /// - Begin servicing game messages.
    /// Once the listener errors out, attempt to join each servicer.
    pub fn listen(
        tcp_addr: SocketAddrV4,
        to_application: mpsc::Sender<msg::ServicerMessage>,
        from_application: spmc::Receiver<msg::ServerReliableMessage>,
    ) -> Result<(), msg::CommError> {
        let listener = TcpListener::bind(tcp_addr).map_err(|err| {
            msg::CommError::FailedToBind(err)
        })?;
        let mut servicer_threads = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Err(_) => {
                    // TODO Log errors.
                    continue;
                }
                Ok(stream) => {
                    let servicer_to_application = to_application.clone();
                    let servicer_from_application = from_application.clone();
                    let servicer_thread = std::thread::spawn(move || {
                        // TODO pass errors to parent thread?
                        ClientReliableServicer::new(
                            stream,
                            servicer_to_application,
                            servicer_from_application,
                        ).unwrap()
                            .spin()
                            .unwrap()
                    });
                    servicer_threads.push(servicer_thread);
                }
            }
        }
        for servicer_thread in servicer_threads.drain(..) {
            servicer_thread.join().unwrap();
        }
        Ok(())
    }

    pub fn new(
        mut tcp_stream: TcpStream,
        to_application: mpsc::Sender<msg::ServicerMessage>,
        from_application: spmc::Receiver<msg::ServerReliableMessage>,
    ) -> Result<Self, msg::CommError> {
        let cred = ClientReliableServicer::wait_for_join_request(&mut tcp_stream)?;

        // Authenticate the user.
        let mut client_data = ClientReliableServicer::authenticate_user(cred)?;

        // Make the servicer.
        let mut servicer = ClientReliableServicer {
            tcp_stream: tcp_stream,
            to_application: to_application,
            from_application: from_application,
            user: client_data.username.clone(),
        };

        // Send client_data to the application thread.
        let client_data_msg = msg::ServicerMessage {
            from: client_data.username.clone(),
            payload: msg::ServicerPayload::ClientData(client_data),
        };
        servicer.to_application.send(client_data_msg).map_err(|_| {
            msg::CommError::ApplicationThreadDisconnected
        })?;

        Ok(servicer)
    }

    pub fn spin(&mut self) -> Result<(), msg::CommError> {
        // Receive inputs from anyone.
        let mut buf = Vec::<u8>::new();
        self.tcp_stream.read(&mut buf).map_err(|err| {
            msg::CommError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        let client_message: msg::ClientReliableMessage =
            bincode::deserialize(&buf).map_err(|err| {
                msg::CommError::FailedToDeserialize(err)
            })?;

        let response = match client_message {
            msg::ClientReliableMessage::ChatMessage(msg) => self.handle_chat_message(msg),
            msg::ClientReliableMessage::JoinRequest(cred) => Err(msg::CommError::AlreadyConnected),
            msg::ClientReliableMessage::None => Ok(msg::ServerReliableMessage::None),
        }?;

        // Serialize the tick.
        let encoded_response = bincode::serialize(&response).map_err(|err| {
            msg::CommError::FailedToSerializeAck(err)
        })?;

        // Send the tick.
        self.tcp_stream.write_all(&encoded_response).map_err(
            |err| {
                msg::CommError::FailedToSendAck(err)
            },
        )?;

        Ok(())
    }

    fn expect_join_request(buf: &Vec<u8>) -> Result<msg::Credentials, msg::CommError> {
        let client_message: msg::ClientReliableMessage =
            bincode::deserialize(&buf).map_err(|err| {
                msg::CommError::FailedToDeserialize(err)
            })?;

        match client_message {
            msg::ClientReliableMessage::JoinRequest(cred) => Ok(cred),
            _ => Err(msg::CommError::WrongMessageType),
        }
    }

    fn wait_for_join_request(
        tcp_stream: &mut TcpStream,
    ) -> Result<msg::Credentials, msg::CommError> {
        loop {
            let mut buf = Vec::<u8>::new();
            match tcp_stream.read(&mut buf) {
                Ok(num_bytes) => {
                    return ClientReliableServicer::expect_join_request(&mut buf);
                }
                Err(err) => {
                    if err.kind() != std::io::ErrorKind::Interrupted {
                        return Err(msg::CommError::FailedToRead(err));
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
    ) -> Result<msg::ServerReliableMessage, msg::CommError> {
        // Send the chat message back to the application thread for broadcast.
        self.to_application
            .send(msg::ServicerMessage {
                from: self.user.clone(),
                payload: msg::ServicerPayload::ChatMessage(msg),
            })
            .map_err(|_| msg::CommError::ApplicationThreadDisconnected)?;

        // No feedback necessary.
        Ok(msg::ServerReliableMessage::None)
    }

    // fn handle_join_request<'de, StateT>(
    //     &mut self,
    //     cred: msg::Credentials,
    // ) -> Result<msg::ServerReliableMessage, msg::CommError>
    // where
    //     StateT: serde::Deserialize<'de>,
    //     StateT: serde::Serialize,
    // {
    //     // Authenticate the user before proceeding.
    //     if !self.authenticate_user(&cred) {
    //         // From the server's perspective, this isn't an error. It's request that deserves a
    //         // negative response.
    //         return Ok(msg::ServerReliableMessage::JoinResponse(
    //             msg::JoinResponse::AuthenticationError,
    //         ));
    //     }

    //     let mut disconnect_user: Option<msg::Username> = None;
    //     if let Some(user) = self.address_user.get_by_first(&src) {
    //         // The source address is already associated with a user.
    //         if *user == cred.username {
    //             // The user is already connected. Just send back a confirmation. This might happen
    //             // if the user hasn't received their confirmation from a previous attempt.
    //             return Ok(msg::ServerReliableMessage::JoinResponse(
    //                 msg::JoinResponse::Confirmation,
    //             ));
    //         } else {
    //             // The source address holder seems to be changing their username but hasn't
    //             // disconnected first. Boot `user` and reconnect.
    //             disconnect_user = Some(*user);
    //         }
    //     }

    //     if let Some(user) = disconnect_user {
    //         self.disconnect_user(&user);
    //     }

    //     self.connect_user(&src, &cred.username);
    //     Ok(msg::ServerReliableMessage::JoinResponse(
    //         msg::JoinResponse::Confirmation,
    //     ))
    // }
}
