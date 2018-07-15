use control;
use msg;
use std;
use bincode;
use bitvec;
use serde;
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::io::Read;
use util;

mod reliable;
mod low_latency;

pub struct Client {
    /// All controller input ticks not yet acknowledged by the server.
    seq: control::ControllerSequence,

    /// A complete history of all chat messages received by this client.
    chat_history: Vec<msg::reliable::ChatMessage>,

    /// The most recent world state sent from the server.
    world_state: Option<Vec<u8>>,

    /// When true, the servicers should disconnect and shutdown. Likewise, if the servicers
    /// disconnect, this should be set to true.
    shutdown: bool,

    from_reliable_servicer: mpsc::Receiver<reliable::ServicerMessage>,
    to_reliable_servicer: mpsc::Sender<reliable::ApplicationMessage>,

    from_low_latency_servicer: mpsc::Receiver<low_latency::ServicerMessage>,
    to_low_latency_servicer: mpsc::Sender<low_latency::ApplicationMessage>,
}

impl Client {
    /// Creates a valid Server connection. Upon success, generates a new Client object.
    pub fn connect(
        username: String,
        password: String,
        mut server_addr: SocketAddr,
    ) -> Result<Self, msg::CommError> {
        let (re_to_application, from_re_servicer) = mpsc::channel();
        let (to_re_servicer, re_from_application) = mpsc::channel();

        // Grab a UDP socket so we can setup our credentials. Don't bother connecting it to the
        // server address yet. This will be done by the low-latency servicer.
        let udp_socket = {
            let raw_udp_addr = SocketAddr::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
                0,
            );
            UdpSocket::bind(raw_udp_addr).map_err(|err| msg::CommError::from(err))
        }?;
        let local_udp_addr = udp_socket.local_addr()?;

        // Setup the credentials.
        let cred = msg::Credentials {
            username: msg::Username(username),
            password: password,
            udp_port: local_udp_addr.port(),
        };

        // Setup the reliable servicer.
        let tcp_stream = TcpStream::connect(&server_addr)?;
        let mut reliable_servicer = reliable::Servicer::connect(
            cred,
            server_addr.clone(),
            tcp_stream,
            re_to_application,
            re_from_application,
        )?;

        // The result is expected to be present after connect() returns in success.
        let server_udp_port = reliable_servicer.server_udp_port().unwrap();
        server_addr.set_port(server_udp_port);

        // Setup the low-latency servicer.
        let (ll_to_application, from_ll_servicer) = mpsc::channel();
        let (to_ll_servicer, ll_from_application) = mpsc::channel();
        let mut low_latency_servicer = low_latency::Servicer::connect(
            udp_socket,
            server_addr,
            ll_to_application,
            ll_from_application,
        )?;

        // Spin up the servicer threads.
        std::thread::spawn(move || reliable_servicer.spin());
        std::thread::spawn(move || low_latency_servicer.spin());

        // Put the client object together.
        Ok(Client {
            seq: control::ControllerSequence::new(),
            chat_history: vec![],
            world_state: None,
            shutdown: false,
            from_reliable_servicer: from_re_servicer,
            to_reliable_servicer: to_re_servicer,
            from_low_latency_servicer: from_ll_servicer,
            to_low_latency_servicer: to_ll_servicer,
        })
    }

    /// Disconnect all servicer queues, which tells their threads to join.
    /// This method consumes the Client object it is called on, ensuring no other methods can be
    /// called on it.
    pub fn quit(mut self) {
        // Disconnect servicer message queues. This informs the servicers to disconnect from their
        // sockets and join their parent thread.
        drop(self.from_reliable_servicer);
        drop(self.to_reliable_servicer);
        drop(self.from_low_latency_servicer);
        drop(self.to_low_latency_servicer);
    }

    /// If the servicer is still connected, we add `controller_input` to the list of unacknowledged
    /// controller inputs. The servicer handles the compression, network transmission, and
    /// receipt of acknowledgment for any inputs the server has received.
    ///
    /// Returns `false` if there is no connection to the server or if there was a problem
    /// compressing the control sequence. Otherwise returns `true`.
    pub fn send_controller_input(
        &mut self,
        controller_input: bitvec::BitVec,
    ) -> Result<(), msg::ApplicationError> {
        if self.shutdown {
            return Err(msg::ApplicationError::Shutdown);
        }

        let msg = low_latency::ApplicationMessage::Control(controller_input);
        if self.to_low_latency_servicer.send(msg).is_err() {
            self.shutdown = true;
            Err(msg::ApplicationError::Shutdown)
        } else {
            Ok(())
        }
    }

    /// Sends a chat message to the server.
    pub fn send_chat_message(&mut self, chat_msg: String) -> Result<(), msg::ApplicationError> {
        if self.shutdown {
            return Err(msg::ApplicationError::Shutdown);
        }

        // Send the chat string to the reliable servicer, who forwards it to the server. The server
        // is responsible for populating the username attached to the chat message.
        let msg = reliable::ApplicationMessage::ChatMessage(chat_msg);
        if self.to_reliable_servicer.send(msg).is_err() {
            // Our reliable servicer queue has been disconnected. Time to shutdown.
            self.shutdown = true;
            Err(msg::ApplicationError::Shutdown)
        } else {
            Ok(())
        }
    }

    /// Consumes the latest world-state (currently just an opaque byte vector) sent to us from the
    /// server.
    pub fn take_world_state(&mut self) -> Result<Vec<u8>, msg::ApplicationError> {
        if self.shutdown {
            return Err(msg::ApplicationError::Shutdown);
        }

        // Drain the incoming message queue from our low latency servicer before giving away our
        // world state (which comes from the low latency servicer).
        let msg_vec = util::drain_mpsc_receiver(&mut self.from_low_latency_servicer)
            .map_err(|err| {
                self.shutdown = true;
                msg::ApplicationError::Shutdown
            })?;

        for msg in msg_vec {
            match msg {
                low_latency::ServicerMessage::WorldState(world_state) => {
                    self.world_state = Some(world_state);
                }
            }
        }

        // Use a swap trick to give away our internal data without invalidating its variable.
        let mut world_state = None;
        std::mem::swap(&mut self.world_state, &mut world_state);

        match world_state {
            Some(ws) => Ok(ws),
            None => Ok(vec![]),
        }
    }

    /// Consumes all previously unconsumed chat history from the server.
    pub fn take_chat_history(
        &mut self,
    ) -> Result<Vec<msg::reliable::ChatMessage>, msg::ApplicationError> {
        if self.shutdown {
            return Err(msg::ApplicationError::Shutdown);
        }

        // Drain the incoming message queue from our reliable servicer before giving away our chat
        // history (which comes from the reliable servicer).
        let msg_vec = util::drain_mpsc_receiver(&mut self.from_reliable_servicer)
            .map_err(|err| {
                self.shutdown = true;
                msg::ApplicationError::Shutdown
            })?;

        for msg in msg_vec {
            match msg {
                reliable::ServicerMessage::ChatMessage(chat) => {
                    self.chat_history.push(chat);
                }
            }
        }

        // Use a swap trick to give away our internal data without invalidating its variable.
        let mut chat_history = vec![];
        std::mem::swap(&mut self.chat_history, &mut chat_history);

        Ok(chat_history)
    }
}
