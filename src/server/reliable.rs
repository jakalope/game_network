use server;
use server::low_latency;
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
use std::sync::atomic::{AtomicBool, Ordering};

/// Services a single client using a reliable transport (Tcp). Communicates messages to the
/// application thread via an in-process queue.
pub struct Servicer {
    /// Our reliable transport to/from the client.
    tcp_stream: TcpStream,

    /// During `spin()`, we check this value to see if we should stop spinning.
    server_running: std::sync::Arc<AtomicBool>,

    /// Our in-process transport to the application thread.
    to_application: mpsc::Sender<server::ServicerMessage>,

    /// Our in-process transport from the application thread to all reliable servicers.
    from_application: spmc::Receiver<msg::reliable::ServerMessage>,

    /// The user this servicer is servicing.
    user: msg::Username,

    /// When `true`, the server will terminate any open connections and stop servicing inputs.
    shutting_down: bool,
}

fn listen<F, T>(
    tcp_listener: TcpListener,
    server_running: std::sync::Arc<AtomicBool>,
    fun: F,
) -> std::io::Result<()>
where
    F: Fn(TcpStream) -> std::thread::JoinHandle<T>,
    F: Send + 'static,
    T: Send + 'static,
{
    let mut servicer_threads = Vec::new();
    tcp_listener.set_nonblocking(true)?;
    for stream in tcp_listener.incoming() {
        match stream {
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // wait until network socket is ready, typically implemented
                // via platform-specific APIs such as epoll or IOCP
                error!("TcpListener would block. Sleeping for one second.");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            Err(err) => {
                comm_log!(msg::CommError::Drop(msg::Drop::IoFailure(err)));
            }
            Ok(stream) => {
                servicer_threads.push(fun(stream));
            }
        }

        // Rest for half a second between accepting successive Tcp connections.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // See if the server still wants us to listen for connections.
        if server_running.load(Ordering::Relaxed) == false {
            break;
        }
    }

    for servicer_thread in servicer_threads.drain(..) {
        servicer_thread.join().unwrap();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_start_stop() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut handle: std::thread::JoinHandle<std::io::Result<()>>;
        let mut server_running = std::sync::Arc::new(AtomicBool::new(true));

        let running_clone = server_running.clone();
        handle = std::thread::spawn(|| {
            listen(listener, running_clone, |_| std::thread::spawn(|| {}))
        });

        std::thread::sleep(std::time::Duration::from_millis(100));

        // Let "server_running" be false, causing the listener thread to join.
        server_running.store(false, Ordering::Relaxed);

        assert!(handle.join().unwrap().is_ok());
    }

    #[test]
    fn listener_connect_nonblocking() {
        let listener = TcpListener::bind("127.0.0.1:29484").unwrap();
        let mut handle: std::thread::JoinHandle<std::io::Result<()>>;
        let mut server_running = std::sync::Arc::new(AtomicBool::new(true));
        let running_clone = server_running.clone();

        handle = std::thread::spawn(|| {
            listen(listener, running_clone, |_| std::thread::spawn(|| {}))
        });
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Attempt to make a connection.
        let tcp_stream = TcpStream::connect("127.0.0.1:29484").unwrap();

        // Let "server_running" be false, causing the listener thread to join.
        server_running.store(false, Ordering::Relaxed);
        assert!(handle.join().unwrap().is_ok());
    }
}

impl Servicer {
    /// Upon new incoming connection, create a reliable servicer:
    /// - Clone to_application.
    /// - Clone reliable from_application.
    /// From the reliable servicer:
    /// - Authenticate the new connection.
    /// - Establish a client-side UDP socket.
    /// - Begin servicing game messages.
    /// Once the "server_running" goes out of scope, the listener will stop accepting new
    /// connections and wait for each servicer to join for it joins the calling thread.
    pub fn listen(
        tcp_listener: TcpListener,
        to_application: mpsc::Sender<server::ServicerMessage>,
        from_application: spmc::Receiver<msg::reliable::ServerMessage>,
    ) -> (std::sync::Arc<AtomicBool>, std::thread::JoinHandle<std::io::Result<()>>) {
        let server_running = std::sync::Arc::new(AtomicBool::new(true));
        let running_clone = server_running.clone();
        let join_handle = std::thread::spawn(|| {
            listen(tcp_listener, running_clone.clone(), move |stream| {
                let to_app = to_application.clone();
                let from_app = from_application.clone();
                let server_running = running_clone.clone();
                std::thread::spawn(move || match Servicer::new(
                    stream,
                    server_running,
                    to_app,
                    from_app,
                ) {
                    Ok(mut servicer) => {
                        servicer.spin();
                    }
                    Err(err) => {
                        log_and_exit!(err);
                    }
                })
            })
        });
        (server_running, join_handle)
    }

    pub fn new(
        mut tcp_stream: TcpStream,
        server_running: std::sync::Arc<AtomicBool>,
        to_application: mpsc::Sender<server::ServicerMessage>,
        from_application: spmc::Receiver<msg::reliable::ServerMessage>,
    ) -> Result<Self, msg::CommError> {
        let cred = Servicer::wait_for_join_request(&mut tcp_stream)?;

        // Authenticate the user.
        let mut client_data = Servicer::authenticate_user(cred)?;

        // Make the servicer.
        let mut servicer = Servicer {
            tcp_stream: tcp_stream,
            server_running: server_running,
            to_application: to_application,
            from_application: from_application,
            user: client_data.username.clone(),
            shutting_down: false,
        };

        // Send client_data to the application thread.
        let client_data_msg = server::ServicerMessage {
            from: client_data.username.clone(),
            payload: server::ServicerPayload::ClientData(client_data),
        };
        servicer.to_application.send(client_data_msg).map_err(|_| {
            msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
        })?;

        Ok(servicer)
    }

    pub fn spin(&mut self) {
        while !self.shutting_down {
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
                        }
                    }
                }
            }
            if self.server_running.load(Ordering::Relaxed) == false {
                self.shutdown();
            }
        }
    }

    fn spin_once(&mut self) -> Result<(), msg::CommError> {
        let mut msg_vec = self.poll_application_thread();
        for msg in msg_vec.drain(..) {
            let encoded_response = bincode::serialize(&msg).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToSerialize(err))
            })?;
            if let Err(err) = self.tcp_stream.write_all(&encoded_response) {
                match msg::CommError::from(err) {
                    msg::CommError::Warning(warn) => {
                        warn!("{:?}: {:?}", self.user, warn);
                    }
                    msg::CommError::Drop(drop) => {
                        error!("{:?}: {:?}", self.user, drop);
                        return Err(msg::CommError::Drop(drop));
                    }
                }
            }
        }

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
            .send(server::ServicerMessage {
                from: self.user.clone(),
                payload: server::ServicerPayload::ChatMessage(msg),
            })
            .map_err(|_| {
                msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
            })?;

        // No feedback necessary.
        Ok(msg::reliable::ServerMessage::None)
    }
}