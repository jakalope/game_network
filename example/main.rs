extern crate game_network;

fn main() {
    type StateT = i32;
    let udp_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let tcp_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut server = game_network::server::Server::<StateT>::new(udp_socket, tcp_listener);
}
