use bincode;
use control;
use msg;
use serde;
use std::io::Read;
use std::net::{TcpStream, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std;

#[derive(PartialEq)]
enum Spin {
    Loop,
    Exit,
}

struct Servicer {
    tcp_stream: TcpStream,
    to_application: mpsc::Sender<msg::reliable::ServerMessage>,
}

impl Servicer {
    // let mut tcp_stream = TcpStream::connect(&server_addr)?;
    // let (to_application, from_application) = mpsc::channel();
    pub fn new(
        self_addr: SocketAddrV4,
        server_addr: SocketAddrV4,
        tcp_stream: TcpStream,
        to_application: mpsc::Sender<msg::reliable::ServerMessage>,
    ) -> Self {
        Servicer {
            tcp_stream: tcp_stream,
            to_application: to_application,
        }
    }

    pub fn spin(&mut self) -> Result<(), msg::CommError> {
        while self.spin_once()? == Spin::Loop {}
        Ok(())
    }

    fn spin_once(&mut self) -> Result<Spin, msg::CommError> {
        // Receive ACK only from the connected server.
        let mut buf = Vec::<u8>::new();
        let bytes = self.tcp_stream.read(&mut buf).map_err(|err| {
            msg::CommError::from(err)
        })?;

        if bytes == 0 {
            // The Tcp stream has been closed.
            // Do more client management here.
            return Ok(Spin::Exit);
        }

        // Deserialize the received datagram.
        // The ACK contains the latest controller input the server has received from us.
        let server_message: msg::reliable::ServerMessage =
            bincode::deserialize(&buf[..]).map_err(|err| {
                msg::CommError::Warning(msg::Warning::FailedToDeserialize(err))
            })?;

        match server_message {
            msg::reliable::ServerMessage::JoinResponse(response) => {
                self.handle_join_response(response)?;
            }
            msg::reliable::ServerMessage::ChatMessage(chat) => {
                self.handle_chat_message(chat)?;
            }
            msg::reliable::ServerMessage::LastTickReceived(tick) => {
                self.handle_last_tick_received(tick)?;
            }
        }

        Ok(Spin::Loop)
    }

    fn handle_last_tick_received(&mut self, last_tick: usize) -> Result<(), msg::CommError> {
        // TODO this needs to be handled in the application thread in order to sync with the
        // low latency servicer.
        // Now we remove the controller inputs the server has ACK'd.
        // self.controller_seq.remove_till_tick(last_tick + 1);
        self.to_application
            .send(msg::reliable::ServerMessage::LastTickReceived(last_tick))
            .map_err(|err| {
                msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
            })
    }

    fn handle_join_response(
        &self,
        response: msg::reliable::JoinResponse,
    ) -> Result<(), msg::CommError> {
        Ok(())
    }

    fn handle_chat_message(
        &mut self,
        chat: msg::reliable::ChatMessage,
    ) -> Result<(), msg::CommError> {
        self.to_application
            .send(msg::reliable::ServerMessage::ChatMessage(chat))
            .map_err(|err| {
                msg::CommError::Drop(msg::Drop::ApplicationThreadDisconnected)
            })
    }
}
