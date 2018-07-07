use bincode;
use control;
use msg;
use serde;
use std::io::{Read, Write};
use std::net::{TcpStream, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std;

fn receive_server_message(
    tcp_stream: &mut TcpStream,
) -> Result<msg::reliable::ServerMessage, msg::CommError> {
    // Receive ACK only from the connected server.
    let mut buf = Vec::<u8>::new();
    let bytes = tcp_stream.read(&mut buf).map_err(
        |err| msg::CommError::from(err),
    )?;

    if bytes == 0 {
        // The Tcp stream has been closed.
        return Err(msg::CommError::Exit);
    }

    // Deserialize the received datagram.
    bincode::deserialize(&buf[..]).map_err(|err| {
        msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
    })
}

pub struct Servicer {
    tcp_stream: TcpStream,
    to_application: mpsc::Sender<msg::reliable::ServerMessage>,
    from_application: mpsc::Receiver<msg::reliable::ClientMessage>,
    server_udp_port: Option<u16>,
}

impl Servicer {
    pub fn connect(
        cred: msg::Credentials,
        server_addr: SocketAddr,
        mut tcp_stream: TcpStream,
        to_application: mpsc::Sender<msg::reliable::ServerMessage>,
        from_application: mpsc::Receiver<msg::reliable::ClientMessage>,
    ) -> Result<Self, msg::CommError> {
        // Create, serialize, and send a join request to the server.
        let request = msg::reliable::ClientMessage::JoinRequest(cred);

        let encoded_request: Vec<u8> = bincode::serialize(&request).map_err(|err| {
            msg::CommError::Warning(msg::Warning::FailedToSerialize(err))
        })?;

        tcp_stream.write(&encoded_request).map_err(|err| {
            msg::CommError::from(err)
        })?;

        // Create the servicer.
        let mut servicer = Servicer {
            tcp_stream: tcp_stream,
            to_application: to_application,
            from_application: from_application,
            server_udp_port: None,
        };

        // Wait for the server's join response.
        loop {
            match servicer.spin_once() {
                Ok(()) => { if let Some(_) = servicer.server_udp_port() { return Ok(servicer);} }
                Err(e) => {
                    return Err(e);
                }
            }
        }
    }

    pub fn server_udp_port(&self) -> Option<u16> {
        return self.server_udp_port;
    }

    pub fn spin(&mut self) {
        while let Ok(_) = self.spin_once() {
            // There is no reason this thread should need to operate faster than 60Hz.
            std::thread::sleep(std::time::Duration::from_millis((1000.0 / 60.0) as i32));
        }
    }

    fn spin_once(&mut self) -> Result<(), msg::CommError> {
        match receive_server_message(&mut self.tcp_stream)? {
            msg::reliable::ServerMessage::JoinResponse(response) => {
                self.handle_join_response(response)
            }
            msg::reliable::ServerMessage::ChatMessage(chat) => self.handle_chat_message(chat),
        }
    }

    fn handle_join_response(
        &mut self,
        response: msg::reliable::JoinResponse,
    ) -> Result<(), msg::CommError> {
        match response {
            msg::reliable::JoinResponse::Confirmation(port) => {
                self.server_udp_port = Some(port);
                return Ok(());
            }
            msg::reliable::JoinResponse::AuthenticationError => {
                return Err(msg::CommError::Exit);
            }
        }
    }

    fn handle_chat_message(
        &mut self,
        chat: msg::reliable::ChatMessage,
    ) -> Result<(), msg::CommError> {
        self.to_application.send(chat).map_err(|err| {
            msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
        })
    }
}
