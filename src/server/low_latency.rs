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
use super::drain_receiver;


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

impl<StateT> ToClient<StateT>
where
    StateT: serde::Serialize,
{
    pub fn new(to: msg::Username, payload: msg::low_latency::ServerMessage<StateT>) -> Self {
        ToClient {
            to: to,
            payload: payload,
        }
    }
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
    // We could use a job queue and generate N low latency servicers.
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
        let mut to_client = None;
        match drain_receiver(&mut self.from_application) {
            Ok(mut msg_vec) => {
                to_client = parse_application_messages(msg_vec, &mut self.address_user);
            }
            Err(drop) => {
                return Err(msg::CommError::Drop(drop));
            }
        }
        if let Some(msg) = to_client {
            send_message(&self.address_user, &self.udp_socket, msg)?;
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
                self.handle_controller_input(src, input)
            }
            msg::low_latency::ClientMessage::None => {
                return Ok(());
            }
        }?;

        if let Err(_) = self.to_application.send(response) {
            // Application thread closed the in-process connection, so we should exit gracefully.
            self.shutdown();
        }
        Ok(())
    }

    fn shutdown(&mut self) {
        self.shutting_down = true;
    }

    fn handle_controller_input(
        &mut self,
        src: std::net::SocketAddr,
        input: control::CompressedControllerSequence,
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

// Parse a vector of app messages, putting ToClient into to_client_vec and updating address_user on
// NewClient or ClientDisconnect.
fn parse_application_messages<StateT>(
    mut msg_vec: Vec<ApplicationMessage<StateT>>,
    address_user: &mut bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,
) -> Option<ToClient<StateT>>
where
    StateT: serde::Serialize,
{
    let mut to_client_opt = None;
    for msg in msg_vec.drain(..) {
        match msg {
            ApplicationMessage::ToClient(to_client) => {
                // We should only store the most recent of each variant.
                // Since at the moment we only have a WorldState, this is trivial.
                match to_client.payload {
                    msg::low_latency::ServerMessage::WorldState(_) => {
                        to_client_opt = Some(to_client);
                    }
                    _ => {}
                }
            }
            ApplicationMessage::NewClient(client_data) => {
                address_user.insert(
                    std::net::SocketAddr::V4(client_data.udp_addr),
                    client_data.username,
                );
            }
            ApplicationMessage::ClientDisconnect(user) => {
                address_user.remove_by_second(&user);
            }
        }
    }
    to_client_opt
}

fn send_message<StateT>(
    address_user: &bidir_map::BidirMap<std::net::SocketAddr, msg::Username>,
    udp_socket: &UdpSocket,
    msg: ToClient<StateT>,
) -> Result<(), msg::CommError>
where
    StateT: serde::Serialize,
{
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
        type StateT = i32;
        let msg_vec = vec![
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(12345),
            )),
            ApplicationMessage::NewClient(msg::ClientData {
                username: msg::Username(String::from("client_data")),
                udp_addr: "127.0.0.1:12345".parse().unwrap(),
            }),
        ];
        let mut address_user = bidir_map::BidirMap::new();
        let to_client_opt = parse_application_messages(msg_vec, &mut address_user);
        assert!(to_client_opt.is_some());
        assert!(!address_user.is_empty());
    }

    #[test]
    fn test_parse_app_msgs_repeated() {
        type StateT = i32;
        let msg_vec = vec![
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(12345),
            )),
            ApplicationMessage::ToClient(ToClient::new(
                msg::Username(String::from("to_client")),
                msg::low_latency::ServerMessage::WorldState(54321),
            )),
        ];
        let mut address_user = bidir_map::BidirMap::new();
        let to_client_opt = parse_application_messages(msg_vec, &mut address_user);
        assert_eq!(
            msg::low_latency::ServerMessage::WorldState(54321),
            to_client_opt.unwrap().payload
        );
    }

    #[test]
    fn test_parse_app_msgs_disconnect() {
        type StateT = i32;
        let msg_vec =
            vec![
                ApplicationMessage::NewClient::<StateT>(msg::ClientData {
                    username: msg::Username(String::from("client_data")),
                    udp_addr: "127.0.0.1:12345".parse().unwrap(),
                }),
                ApplicationMessage::ClientDisconnect(msg::Username(String::from("client_data"))),
            ];
        let mut address_user = bidir_map::BidirMap::new();
        let to_client_opt = parse_application_messages(msg_vec, &mut address_user);
        assert!(to_client_opt.is_none());
        assert!(address_user.is_empty());
    }

    #[test]
    fn test_send_message() {
        type StateT = i32;

        let mut socket = UdpSocket::bind("127.0.0.1:0").unwrap();

        let mut address_user = bidir_map::BidirMap::new();
        address_user.insert(
            socket.local_addr().unwrap(),
            msg::Username(String::from("client_data")),
        );

        let msg = ToClient {
            to: msg::Username(String::from("client_data")),
            payload: msg::low_latency::ServerMessage::WorldState(12345),
        };
        send_message(&address_user, &socket, msg).unwrap();

        let mut buf = [0; 1500];
        socket.recv_from(&mut buf).unwrap();

        let server_message: msg::low_latency::ServerMessage<StateT> =
            bincode::deserialize(&buf[..]).unwrap();

        assert_eq!(
            msg::low_latency::ServerMessage::WorldState(12345),
            server_message
        );
    }
}
