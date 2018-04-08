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
    ///
    ClientData(msg::ClientData),
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

pub struct Server<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    low_latency_servicer: low_latency::Servicer<StateT>,
    reliable_join_handle: std::thread::JoinHandle<()>,
    from_servicer: mpsc::Receiver<ServicerMessage>,
    to_low_latency_servicer: spmc::Sender<low_latency::ApplicationMessage<StateT>>,
    // TODO consider making a reliable::ApplicationMessage
    to_reliable_servicer: spmc::Sender<reliable::ToClient>,
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

    pub fn send_low_latency(&mut self, msg: low_latency::ApplicationMessage<StateT>) {
        self.to_low_latency_servicer.send(msg);
    }

    pub fn send_reliable(&mut self, msg: reliable::ToClient) {
        self.to_reliable_servicer.send(msg);
    }

    pub fn quit(mut self) -> std::thread::Result<()> {
        // Disconnect servicer message queues. This informs the servicers to disconnect from their
        // sockets and join their parent thread.
        drop(self.to_low_latency_servicer);
        drop(self.to_reliable_servicer);
        self.reliable_join_handle.join()
    }
}

pub fn drain_receiver<M: Send>(receiver: &mut spmc::Receiver<M>) -> Result<Vec<M>, msg::Drop> {
    let mut msgs = vec![];
    loop {
        // Receive inputs from the application thread.
        match receiver.try_recv() {
            Ok(msg) => {
                msgs.push(msg);
            }
            Err(spmc::TryRecvError::Empty) => {
                return Ok(msgs);
            }
            Err(spmc::TryRecvError::Disconnected) => {
                // The application thread disconnected; we should begin shutting down.
                return Err(msg::Drop::ApplicationThreadDisconnected);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_receiver_empty() {
        let (mut to, mut from): (spmc::Sender<AtomicBool>, spmc::Receiver<AtomicBool>) =
            spmc::channel();
        let msgs = drain_receiver(&mut from).expect("spmc disconnected unexpectedly");

        // Expect zero messages to arrive.
        assert_eq!(0, msgs.len());
    }

    #[test]
    fn drain_receiver_nonempty() {
        let (mut to, mut from) = spmc::channel();
        to.send(AtomicBool::new(true)).unwrap();
        let msgs = drain_receiver(&mut from).expect("spmc disconnected unexpectedly");

        // Expect exactly one message with a value of "true" to arrive.
        assert_eq!(1, msgs.len());
        assert_eq!(true, msgs[0].load(Ordering::Relaxed));
    }

    #[test]
    fn drain_receiver_disconnected() {
        let (mut to, mut from) = spmc::channel();
        to.send(AtomicBool::new(true)).unwrap();
        drop(to);

        // Even though we sent something, if the channel has been disconnected, we don't want to
        // process any further.
        assert!(drain_receiver(&mut from).is_err());
    }

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
