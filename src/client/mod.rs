use control;
use msg;
use std;
use bincode;
use serde;
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::io::Read;

mod reliable;
mod low_latency;

pub struct Client<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
    for<'de> StateT: serde::Deserialize<'de>,
{
    /// All controller input ticks not yet acknowledged by the server.
    seq: control::ControllerSequence,

    /// A complete history of all chat messages received by this client.
    chat_history: Vec<msg::ChatMessage>,

    /// The last of our controller input ticks the server has acknowledged.
    /// At 60Hz, this will wrap in 414.25 days. I hope this becomes a problem. In the mean time,
    /// we're trying to save some bandwidth.
    last_tick_ackd: i32,

    /// The most recent world state sent from the server.
    world_state: Option<StateT>,

    /// When true, the servicers should disconnect and shutdown. Likewise, if the servicers
    /// disconnect, this should be set to true.
    shutdown: bool,

    from_reliable_servicer: mpsc::Receiver<msg::reliable::ServerMessage>,
    to_reliable_servicer: mpsc::Sender<msg::reliable::ClientMessage>,
    reliable_spin_handle: std::thread::JoinHandle<()>,

    from_low_latency_servicer: mpsc::Receiver<msg::low_latency::ServerMessage<StateT>>,
    to_low_latency_servicer: mpsc::Sender<msg::low_latency::ClientMessage>,
    low_latency_spin_handle: std::thread::JoinHandle<()>,
}

impl<StateT> Client<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
    for<'de> StateT: serde::Deserialize<'de>,
{
    pub fn connect(
        username: Username,
        password: String,
        mut server_addr: SocketAddr,
    ) -> Result<Self, msg::CommError> {
        let (re_to_application, from_re_servicer) = mpsc::channel();

        // Grab a UDP socket so we can setup our credentials. Don't bother connecting it to the
        // server address yet. This will be done by the low-latency servicer.
        let udp_socket = {
            let raw_udp_addr = "127.0.0.1:0".parse()?;
            UdpSocket::bind(raw_udp_addr).map_err(|err| msg::CommError::from(err))
        }?;
        let local_udp_addr = udp_socket.local_addr()?;

        // Setup the credentials.
        let cred: msg::Credentials = {
            username: username,
            password: password,
            udp_port: local_udp_addr.port(),
        };

        // Setup the reliable servicer.
        let mut tcp_stream = TcpStream::connect(&server_addr)?;
        let reliable_servicer =
            reliable::Servicer::connect(cred, server_addr.clone(), tcp_stream, re_to_application)?;

        // The result is expected to be present after connect() returns in success.
        let server_udp_port = reliable_servicer.server_udp_port().unwrap();
        server_addr.set_port(server_udp_port);

        // Setup the low-latency servicer.
        let (ll_to_application, from_ll_servicer) = mpsc::channel();
        let (to_ll_servicer, ll_from_application) = mpsc::channel();
        let low_latency_servicer = low_latency::Servicer::connect(
                udp_socket, server_addr, to_ll_application, ll_from_application)?;

        // Spin up the servicer threads.
        let reliable_spin_handle = std::thread::spawn(move || reliable_servicer.spin());
        let low_latency_spin_handle = std::thread::spawn(move || low_latency_servicer.spin());

        // Put the client object together.
        Ok(Client {
            seq: control::ControllerSequence::new(),
            chat_history: vec![],
            last_tick_ackd: -1,
            shutdown: false,
            from_reliable_servicer: from_re_servicer,
            reliable_spin_handle: reliable_spin_handle,
            from_low_latency_servicer: from_ll_servicer,
            to_low_latency_servicer: to_ll_servicer,
            low_latency_spin_handle: low_latency_spin_handle,
        })
    }

    fn drain_from_reliable() {
        let msg: msg::reliable::ServerMessage;
        loop {
            match self.from_reliable_servicer.try_recv() {
                Err(TryRecvError::Empty) => {return;},
                Err(TryRecvError::Disconnect) => {
                    self.shutdown = true;
                },
                Some(next_msg) => {
                    msg = next_msg;
                },
            };
            match msg {
                msg::reliable::ServerMessage::JoinResponse(_) => {
                    // Unexpected message type for this context. Ignore.
                },
                msg::reliable::ServerMessage::ChatMessage(chat) => {
                    self.chat_history.push(chat);
                },
                msg::reliable::ServerMessage::LastTickReceived(tick) => {
                    self.last_tick_ackd = tick;
                },
            }
        }
    }

    fn drain_from_low_latency() {
        let msg: msg::low_latency::ServerMessage<StateT>;
        loop {
            match self.from_low_latency_servicer.try_recv() {
                Err(TryRecvError::Empty) => {return;},
                Err(TryRecvError::Disconnect) => {
                    self.shutdown = true;
                },
                Some(next_msg) => {
                    msg = next_msg;
                },
            };
            match msg {
                msg::low_latency::ServerMessage::WorldState(world_state) => {
                    self.world_state = Some(world_state);
                },
            }
        }
    }

    /// First, we drain the low-latency receive buffer. If the servicer is still connected, we then
    /// add `controller_input` to the unacknowledged controller input sequence, compress the
    /// sequence, and send it to the servicer.
    ///
    /// Returns `false` if there is no connection to the server or if there was a problem
    /// compressing the control sequence. Otherwise returns `true`.
    pub fn send_controller_input(&mut self, controller_input: bitvec::BitVec) -> bool {
        // Update the sequence, removing elements already received and ACK'd by the server.
        if !self.drain_from_low_latency() {
            return false;
        }
        self.seq.push(controller_input);
        let compressed_seq = self.seq.to_compressed();
        if compressed_seq.is_none() {
            error!("Failed to compress a control sequence: {:?}", self.seq);
            self.shutdown = true;
            return false;
        }
        let msg = msg::low_latency::ClientMessage::ControllerInput(compressed_seq.unwrap());
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

    pub fn world_state(&self) -> Option<StateT> {
        self.world_state
    }

    pub fn chat_history(&self) -> &Vec<msg::reliable::ChatMessage> {
        self.chat_history
    }
}
