extern crate game_network;

fn main() {
    let udp_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let tcp_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut _server = game_network::server::Server::new(udp_socket, tcp_listener);
}
