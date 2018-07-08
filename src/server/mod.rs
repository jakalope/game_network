mod low_latency;
mod reliable;

use bidir_map;
use bincode;
use control;
use msg;
use serde;
use spmc;
use std::io::{Write, Read};
use std::net::{TcpStream, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std;

/// Represents messages passed from client servicer threads to the main application thread.
#[derive(Clone)]
pub enum ServicerPayload {
    /// A message that will be visible to all players.
    ChatMessage(String),
    /// A series of control inputs -- one for each game tick since the last tick received.
    ControllerSequence(control::ControllerSequence),
    /// Used to authenticate a user.
    ClientData(msg::ClientData),
}

/// Represents messages passed from servicer threads to the main application thread, along
/// with the user the message originated from over the network.
#[derive(Clone)]
pub struct ServicerMessage {
    /// Originator's user name.
    pub from: msg::Username,
    /// The message payload to be handled by the application thread.
    pub payload: ServicerPayload,
}

pub struct Server {
    listener_kill_switch: std::sync::Arc<AtomicBool>,
    listener_join_handle: std::thread::JoinHandle<std::result::Result<(), std::io::Error>>,
    low_latency_servicer: low_latency::Servicer,
    from_servicer: mpsc::Receiver<ServicerMessage>,
    to_low_latency_servicer: spmc::Sender<low_latency::ApplicationMessage>,
    to_reliable_servicer: spmc::Sender<reliable::ApplicationMessage>,
    shutdown: bool,
}

impl Server {
    pub fn new(udp_socket: UdpSocket, tcp_listener: TcpListener) -> Self {
        let (to_application, from_servicer) = mpsc::channel();
        let (to_reliable_servicer, re_from_application) = spmc::channel();
        let (to_low_latency_servicer, ll_from_application) = spmc::channel();

        let (listener_kill_switch, listener_join_handle) = reliable::Servicer::listen(
            tcp_listener,
            to_application.clone(),
            re_from_application,
            udp_socket.local_addr().unwrap().port(),
        );

        let low_latency_servicer =
            low_latency::Servicer::new(udp_socket, to_application, ll_from_application);

        Server {
            listener_kill_switch: listener_kill_switch,
            listener_join_handle: listener_join_handle,
            low_latency_servicer: low_latency_servicer,
            from_servicer: from_servicer,
            to_low_latency_servicer: to_low_latency_servicer,
            to_reliable_servicer: to_reliable_servicer,
            shutdown: false,
        }
    }

    pub fn send_low_latency(&mut self, msg: low_latency::ApplicationMessage) {
        self.to_low_latency_servicer.send(msg);
    }

    pub fn send_reliable(&mut self, msg: reliable::ApplicationMessage) {
        if self.to_reliable_servicer.send(msg).is_err() {
            // Our reliable servicer has disconnected. Time to quit.
            self.shutdown = true;
        }
    }

    pub fn receive_iter(&mut self) -> mpsc::Iter<ServicerMessage> {
        self.from_servicer.iter()
    }

    pub fn quit(mut self) {
        // Tell the TCP listener to shut down.
        self.listener_kill_switch.store(false, Ordering::Relaxed);

        // Disconnect servicer message queues. This informs the servicers to disconnect from their
        // sockets and join their parent thread.
        drop(self.to_low_latency_servicer);
        drop(self.to_reliable_servicer);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_udp() {
        let socket = UdpSocket::bind("127.0.0.1:34254").unwrap();

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
