mod low_latency;
mod reliable;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_udp() {
        let mut socket = UdpSocket::bind("127.0.0.1:34254").unwrap();

        // Receives a single datagram message on the socket. If `buf` is too small to hold
        // the message, it will be cut off.
        let mut buf = [0; 10];
        socket.send_to(&buf, "127.0.0.1:34254").unwrap();
        let (amt, src) = socket.recv_from(&mut buf).unwrap();

        // Redeclare `buf` as slice of the received data and send reverse data back to origin.
        let buf = &mut buf[..amt];
        buf.reverse();
        socket.send_to(buf, &src).unwrap();
    }
}
