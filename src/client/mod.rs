use control;
use msg;
use std;
use bincode;
use serde;
use std::net::{TcpStream, SocketAddrV4, UdpSocket};
use std::sync::mpsc;
use std::io::Read;

mod reliable;
mod low_latency;

#[derive(Clone)]
pub enum ServicerMessage<StateT> {
    /// The latest segment of world state needed by a specific client.
    WorldState(StateT),
    /// A message that will be visible to all players.
    ChatMessage(msg::reliable::ChatMessage),
}

pub struct Client<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    reliable_servicer: reliable::Servicer,
    low_latency_servicer: low_latency::Servicer<StateT>,
}

impl<StateT> Client<StateT>
where
    StateT: serde::Serialize,
    StateT: Send,
{
    fn connect(
        cred: msg::Credentials,
        mut server_addr: SocketAddrV4,
    ) -> Result<Self, msg::CommError> {
        let (re_to_application, from_re_servicer) = mpsc::channel();

        let mut tcp_stream = TcpStream::connect(&server_addr)?;
        let reliable_servicer =
            reliable::Servicer::connect(cred, server_addr.clone(), tcp_stream, re_to_application)?;

        // Wait for a server UDP port.
        loop {
            let msg = from_re_servicer.recv().map_err(|_| {
                msg::CommError::Drop(msg::Drop::ServicerThreadDisconnected)
            })?;
            // match msg {
            // }
            // server_addr.set_port(udp_port);
        }

        let self_addr = "127.0.0.1:0".parse().unwrap();
        let (ll_to_application, from_ll_servicer) = mpsc::channel();
        let low_latency_servicer =
            low_latency::Servicer::connect(self_addr, server_addr, ll_to_application)?;

        Ok(Client {
            reliable_servicer: reliable_servicer,
            low_latency_servicer: low_latency_servicer,
        })
    }
}
