use control;
use msg;
use std;
use bincode;
use bitvec;
use serde;
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::io::Read;

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

    from_reliable_servicer: mpsc::Receiver<msg::reliable::ServerMessage>,
    to_reliable_servicer: mpsc::Sender<msg::reliable::ClientMessage>,
    reliable_spin_handle: std::thread::JoinHandle<()>,

    from_low_latency_servicer: mpsc::Receiver<low_latency::ServicerMessage>,
    to_low_latency_servicer: mpsc::Sender<low_latency::ApplicationMessage>,
    low_latency_spin_handle: std::thread::JoinHandle<()>,
}

impl Client {
    pub fn connect(
        username: msg::Username,
        password: String,
        mut server_addr: SocketAddr,
    ) -> Result<Self, msg::CommError> {
        let (re_to_application, from_re_servicer) = mpsc::channel();
        let (to_re_servicer, re_from_application) = mpsc::channel();

        // Grab a UDP socket so we can setup our credentials. Don't bother connecting it to the
        // server address yet. This will be done by the low-latency servicer.
        let udp_socket = {
            let raw_udp_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            UdpSocket::bind(raw_udp_addr).map_err(|err| msg::CommError::from(err))
        }?;
        let local_udp_addr = udp_socket.local_addr()?;

        // Setup the credentials.
        let cred = msg::Credentials {
            username: username,
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
        let reliable_spin_handle = std::thread::spawn(move || reliable_servicer.spin());
        let low_latency_spin_handle = std::thread::spawn(move || low_latency_servicer.spin());

        // Put the client object together.
        Ok(Client {
            seq: control::ControllerSequence::new(),
            chat_history: vec![],
            world_state: None,
            shutdown: false,
            from_reliable_servicer: from_re_servicer,
            to_reliable_servicer: to_re_servicer,
            reliable_spin_handle: reliable_spin_handle,
            from_low_latency_servicer: from_ll_servicer,
            to_low_latency_servicer: to_ll_servicer,
            low_latency_spin_handle: low_latency_spin_handle,
        })
    }

    fn drain_from_reliable(&mut self) {
        loop {
            match self.from_reliable_servicer.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.shutdown = true;
                }
                Ok(next_msg) => {
                    match next_msg {
                        msg::reliable::ServerMessage::JoinResponse(_) => {
                            // Unexpected message type for this context. Ignore.
                        }
                        msg::reliable::ServerMessage::ChatMessage(chat) => {
                            self.chat_history.push(chat);
                        }
                    }
                }
            };
        }
    }

    fn drain_from_low_latency(&mut self) {
        loop {
            match self.from_low_latency_servicer.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.shutdown = true;
                }
                Ok(next_msg) => {
                    match next_msg {
                        low_latency::ServicerMessage::WorldState(world_state) => {
                            self.world_state = Some(world_state);
                        }
                    }
                }
            };
        }
    }

    /// First, we drain the low-latency receive buffer. If the servicer is still connected, we then
    /// add `controller_input` to the unacknowledged controller input sequence. The servicer handles
    /// the compression and network transmission.
    ///
    /// Returns `false` if there is no connection to the server or if there was a problem
    /// compressing the control sequence. Otherwise returns `true`.
    pub fn send_controller_input(&mut self, controller_input: bitvec::BitVec) -> bool {
        self.drain_from_low_latency();
        if self.shutdown {
            return false;
        }
        let msg = low_latency::ApplicationMessage::Control(controller_input);
        if self.to_low_latency_servicer.send(msg).is_err() {
            self.shutdown = true;
            return false;
        }
        true
    }

    pub fn send_chat_message(&mut self, chat_msg: String) {
        let msg = msg::reliable::ClientMessage::ChatMessage(chat_msg);
        self.to_reliable_servicer.send(msg);
    }

    pub fn take_world_state(&mut self) -> Option<Vec<u8>> {
        let mut world_state = None;
        std::mem::swap(&mut self.world_state, &mut world_state);
        world_state
    }

    pub fn take_chat_history(&mut self) -> Vec<msg::reliable::ChatMessage> {
        let mut chat_history = vec![];
        std::mem::swap(&mut self.chat_history, &mut chat_history);
        chat_history
    }
}
