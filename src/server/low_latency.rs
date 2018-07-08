use server;
use server::reliable;
use msg;
use control;
use std;
use std::net::{TcpStream, TcpListener, UdpSocket};
use std::sync::mpsc;
use std::io::{Write, Read};
use serde;
use bincode;
use spmc;
use bidir_map;
use util::{drain_spmc_receiver, maybe_receive_udp};


/// Represents a message from the application thread to the low latency servicer, bound for the
/// client.
#[derive(Clone, Debug, PartialEq)]
pub struct ToClient {
    /// The user `payload` is intended for.
    pub to: msg::Username,

    /// The payload intended for the user `to`.
    pub payload: msg::low_latency::ServerMessage,
}

impl ToClient {
    pub fn new(to: msg::Username, payload: msg::low_latency::ServerMessage) -> Self {
        ToClient {
            to: to,
            payload: payload,
        }
    }
}

/// Represents a message from the server application thread to the server low latency servicer.
#[derive(Clone, PartialEq)]
pub enum ApplicationMessage {
    /// A message to be forwarded to a specific client.
    ToClient(ToClient),

    /// When a new client connects, the low latency servicer updates its user/socket map.
    NewClient(msg::ClientData),

    /// When a client disconnects, the low latency servicer updates its user/socket map.
    ClientDisconnect(msg::Username),
}

/// Services all clients using a low-latency transport (Udp). Communicates messages to the
/// application thread via an in-process queue.
///
/// Drop errors on the low latency servicer has different semantics than on the reliable
/// side. Since the low latency servicer handles all clients, Drop means "drop this particular
/// client and propagate this fact to all other threads".
pub struct Servicer {
    /// Our low-latency transport from the client.
    udp_socket: UdpSocket,

    /// Our in-process transport to the application thread.
    to_application: mpsc::Sender<server::ServicerMessage>,

    /// Our in-process transport from the application thread to all low-latency servicers.
    // TODO Do we want more than one low-latency servicer connected to the same port to send/receive
    // concurrently?  We could use a job queue and generate N low latency servicers.
    // https://blog.cloudflare.com/how-to-receive-a-million-packets/
    // https://lwn.net/Articles/542629/
    // https://stackoverflow.com/questions/40468685/how-to-set-the-socket-option-so-reuseport-in-rust
    from_application: spmc::Receiver<ApplicationMessage>,

    address_user: bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,

    last_tick_received: usize,

    /// When `true`, the server will terminate any open connections and stop servicing inputs.
    shutting_down: bool,
}

impl Servicer {
    pub fn new(
        udp_socket: UdpSocket,
        to_application: mpsc::Sender<server::ServicerMessage>,
        from_application: spmc::Receiver<ApplicationMessage>,
    ) -> Self {
        Servicer {
            udp_socket: udp_socket,
            to_application: to_application,
            from_application: from_application,
            address_user: bidir_map::BidirMap::<std::net::SocketAddr, msg::Username>::new(),
            last_tick_received: 0,
            shutting_down: false,
        }
    }

    pub fn spin(&mut self) {
        if let Err(err) = self.udp_socket.set_nonblocking(true) {
            error!("{:?}", err);
        } else {
            loop {
                match self.spin_once() {
                    Ok(()) => {}
                    Err(msg::CommError::Drop(drop)) => {
                        warn!("Drop: {:?}", drop);
                    }
                    Err(msg::CommError::Warning(warn)) => {
                        warn!("Warn: {:?}", warn);
                    }
                    Err(msg::CommError::Exit) => {
                        break;
                    }
                }
            }
        }
        info!("Exiting low latency servicer thread.");
    }

    fn spin_once(&mut self) -> Result<(), msg::CommError> {
        self.forward_from_server_to_client()?;
        self.forward_from_client_to_server()?;
        Ok(())
    }

    // Service the queue of messages coming from the server application thread.
    fn forward_from_server_to_client(&mut self) -> Result<(), msg::CommError> {
        let mut to_client;
        match drain_spmc_receiver(&mut self.from_application) {
            Ok(mut msg_vec) => {
                to_client = parse_application_messages(msg_vec, &mut self.address_user);
            }
            Err(drop) => {
                return Err(msg::CommError::Exit);
            }
        }

        // Send each outgoing message to the client.
        for opt_msg in to_client.into_iter() {
            if let Some(msg) = opt_msg {
                send_message(&self.address_user, &self.udp_socket, msg)?;
            }
        }
        Ok(())
    }

    // Receive inputs from the network.
    fn forward_from_client_to_server(&mut self) -> Result<(), msg::CommError> {
        match maybe_receive_udp(&self.udp_socket) {
            Ok(Some((buf, src))) => self.process_udp(src, &buf),
            Ok(None) => Ok(()),
            Err(err) => Err(err),
        }
    }

    fn process_udp(&mut self, src: std::net::SocketAddr, buf: &[u8]) -> Result<(), msg::CommError> {
        // Deserialize the received datagram.
        let client_message: msg::low_latency::ClientMessage =
            bincode::deserialize(&buf).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
            })?;

        match client_message {
            msg::low_latency::ClientMessage::ControllerInput(input) => {
                self.handle_controller_input(src, input)
            }
        }
    }

    fn handle_controller_input(
        &mut self,
        src: std::net::SocketAddr,
        input: control::CompressedControllerSequence,
    ) -> Result<(), msg::CommError> {
        // Lookup the user given the source socket address.
        let user = self.address_user.get_by_first(&src).ok_or(
            msg::CommError::Warning(msg::Warning::UnknownSource(src)),
        )?;

        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            msg::CommError::Warning(
                msg::Warning::FailedToDecompress,
            ),
        )?;

        // Update the latest controller sequence tick received.
        if controller_seq.last_tick() > self.last_tick_received {
            self.last_tick_received = controller_seq.last_tick();

            // Send the updated controller sequence to the server application thread.
            let to_server_application = server::ServicerMessage {
                from: user.clone(),
                payload: server::ServicerPayload::ControllerSequence(controller_seq),
            };

            self.to_application.send(to_server_application).map_err(
                // Application thread closed the in-process connection, so we should exit gracefully.
                |_| msg::CommError::Exit,
            )?;
        }

        // Send an ACK of the latest tick received back to the client.
        let ack_msg = ToClient::new(
            user.clone(),
            msg::low_latency::ServerMessage::LastTickReceived(self.last_tick_received),
        );
        send_message(&self.address_user, &self.udp_socket, &ack_msg)
    }
}

// Parse a vector of app messages, putting ToClient into to_client_vec and updating address_user on
// NewClient or ClientDisconnect.
fn parse_application_messages(
    mut msg_vec: Vec<ApplicationMessage>,
    address_user: &mut bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,
) -> [Option<ToClient>; 2] {
    const WORLD_STATE: usize = 0;
    const LAST_TICK: usize = 1;
    let mut outgoing = [None, None];

    for msg in msg_vec.drain(..) {
        match msg {
            ApplicationMessage::ToClient(to_client) => {
                // We should only store the most recent of each variant.
                match to_client.payload {
                    msg::low_latency::ServerMessage::WorldState(_) => {
                        outgoing[WORLD_STATE] = Some(to_client);
                    }
                    msg::low_latency::ServerMessage::LastTickReceived(_) => {
                        outgoing[LAST_TICK] = Some(to_client);
                    }
                }
            }
            ApplicationMessage::NewClient(client_data) => {
                address_user.insert(client_data.udp_addr, client_data.username);
            }
            ApplicationMessage::ClientDisconnect(user) => {
                address_user.remove_by_second(&user);
            }
        }
    }
    outgoing
}

fn send_message(
    address_user: &bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,
    udp_socket: &UdpSocket,
    msg: &ToClient,
) -> Result<(), msg::CommError> {
    // If we have the designated user in our address book, send them the
    // message via Udp. Otherwise, log a warning.
    if let Some(address) = address_user.get_by_second(&msg.to) {
        // Serialize the message payload.
        let encode = bincode::serialize(&msg.payload).map_err(|err| {
            msg::CommError::Warning(msg::Warning::FailedToSerialize(err))
        })?;

        udp_socket.send_to(&encode, address).map_err(|err| {
            msg::CommError::Warning(msg::Warning::IoFailure(err))
        })?;
    } else {
        warn!("No such user: {:?}", msg.to);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_app_msgs() {
        let msg_vec = vec![
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(vec![127]),
            )),
            ApplicationMessage::NewClient(msg::ClientData {
                username: msg::Username(String::from("client_data")),
                udp_addr: "127.0.0.1:12345".parse().unwrap(),
            }),
        ];
        let mut address_user = bidir_map::BidirMap::new();
        let outgoing_msgs = parse_application_messages(msg_vec, &mut address_user);
        let world_state_opt = &outgoing_msgs[0];
        let last_tick = &outgoing_msgs[1];
        assert!(world_state_opt.is_some());
        assert!(last_tick.is_none());
        assert!(!address_user.is_empty());
    }

    #[test]
    fn test_parse_app_msgs_repeated() {
        let msg_vec = vec![
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(vec![127]),
            )),
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::LastTickReceived(2),
            )),
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::LastTickReceived(3),
            )),
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(vec![123]),
            )),
        ];
        let mut address_user = bidir_map::BidirMap::new();
        let outgoing_msgs = parse_application_messages(msg_vec, &mut address_user);
        let world_state_opt = &outgoing_msgs[0];
        let last_tick = &outgoing_msgs[1];

        // Expect the latter of the two world states to have been received.
        assert_eq!(
            &Some(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(vec![123]),
            )),
            world_state_opt
        );

        // Expect the latter of the two ticks-received to have been received.
        assert_eq!(
            &Some(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::LastTickReceived(3),
            )),
            last_tick
        );
    }

    #[test]
    fn test_parse_app_msgs_disconnect() {
        let msg_vec =
            vec![
                ApplicationMessage::NewClient(msg::ClientData {
                    username: msg::Username(String::from("client_data")),
                    udp_addr: "127.0.0.1:12345".parse().unwrap(),
                }),
                ApplicationMessage::ClientDisconnect(msg::Username(String::from("client_data"))),
            ];
        let mut address_user = bidir_map::BidirMap::new();
        let outgoing_msgs = parse_application_messages(msg_vec, &mut address_user);
        let world_state_opt = &outgoing_msgs[0];
        let last_tick = &outgoing_msgs[1];
        assert!(world_state_opt.is_none());
        assert!(last_tick.is_none());
        assert!(address_user.is_empty());
    }

    #[test]
    fn test_send_message() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();

        let mut address_user = bidir_map::BidirMap::new();
        address_user.insert(
            socket.local_addr().unwrap(),
            msg::Username(String::from("client_data")),
        );

        let msg = ToClient {
            to: msg::Username(String::from("client_data")),
            payload: msg::low_latency::ServerMessage::WorldState(vec![127]),
        };
        send_message(&address_user, &socket, &msg).unwrap();

        let mut buf = [0; 1500];
        socket.recv_from(&mut buf).unwrap();

        let server_message: msg::low_latency::ServerMessage = bincode::deserialize(&buf[..])
            .unwrap();

        assert_eq!(
            msg::low_latency::ServerMessage::WorldState(vec![127]),
            server_message
        );
    }
}
