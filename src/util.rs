use msg;
use spmc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std;

pub const MAX_PACKET_SIZE: usize = 1500;

pub fn drain_spmc_receiver<M: Send>(receiver: &mut spmc::Receiver<M>) -> Result<Vec<M>, msg::Drop> {
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

pub fn drain_mpsc_receiver<M: Send>(receiver: &mut mpsc::Receiver<M>) -> Result<Vec<M>, msg::Drop> {
    let mut msgs = vec![];
    loop {
        // Receive inputs from the application thread.
        match receiver.try_recv() {
            Ok(msg) => {
                msgs.push(msg);
            }
            Err(mpsc::TryRecvError::Empty) => {
                return Ok(msgs);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // The application thread disconnected; we should begin shutting down.
                return Err(msg::Drop::ApplicationThreadDisconnected);
            }
        }
    }
}

pub fn maybe_receive_from_udp(
    socket: &std::net::UdpSocket,
) -> Result<Option<(Vec<u8>, std::net::SocketAddr)>, msg::CommError> {
    // Make a buffer of the maximum packet size.
    let mut buf = Vec::<u8>::with_capacity(MAX_PACKET_SIZE);
    buf.resize(MAX_PACKET_SIZE, 0);
    match socket.recv_from(&mut buf) {
        Ok((size, src)) => {
            // Truncate to the size of the packet received.
            buf.resize(size, 0);
            Ok(Some((buf, src)))
        }
        Err(err) => {
            match err.kind() {
                // Timing out probably just means we didn't we didn't receive any data.
                std::io::ErrorKind::WouldBlock => Ok(None),
                std::io::ErrorKind::TimedOut => Ok(None),
                // Other errors are unexpected.
                _ => Err(msg::CommError::Warning(msg::Warning::IoFailure(err))),
            }
        }
    }
}

pub fn maybe_receive_udp(socket: &std::net::UdpSocket) -> Result<Vec<u8>, msg::CommError> {
    // Make a buffer of the maximum packet size.
    let mut buf = Vec::<u8>::with_capacity(MAX_PACKET_SIZE);
    buf.resize(MAX_PACKET_SIZE, 0);
    match socket.recv(&mut buf) {
        Ok(size) => {
            // Truncate to the size of the packet received.
            buf.resize(size, 0);
            Ok(buf)
        }
        Err(err) => {
            match err.kind() {
                // Timing out probably just means we didn't we didn't receive any data.
                std::io::ErrorKind::WouldBlock => Ok(vec![]),
                std::io::ErrorKind::TimedOut => Ok(vec![]),
                // Other errors are unexpected.
                _ => Err(msg::CommError::Warning(msg::Warning::IoFailure(err))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // SPMC tests
    #[test]
    fn drain_receiver_empty() {
        let (mut to, mut from): (spmc::Sender<AtomicBool>, spmc::Receiver<AtomicBool>) =
            spmc::channel();
        let msgs = drain_spmc_receiver(&mut from).expect("spmc disconnected unexpectedly");

        // Expect zero messages to arrive.
        assert_eq!(0, msgs.len());
    }

    #[test]
    fn drain_receiver_nonempty() {
        let (mut to, mut from) = spmc::channel();
        to.send(AtomicBool::new(true)).unwrap();
        let msgs = drain_spmc_receiver(&mut from).expect("spmc disconnected unexpectedly");

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
        assert!(drain_spmc_receiver(&mut from).is_err());
    }

    // MPSC tests
    #[test]
    fn drain_mpsc_receiver_empty() {
        let (mut to, mut from): (mpsc::Sender<AtomicBool>, mpsc::Receiver<AtomicBool>) =
            mpsc::channel();
        let msgs = drain_mpsc_receiver(&mut from).expect("mpsc disconnected unexpectedly");

        // Expect zero messages to arrive.
        assert_eq!(0, msgs.len());
    }

    #[test]
    fn drain_mpsc_receiver_nonempty() {
        let (mut to, mut from) = mpsc::channel();
        to.send(AtomicBool::new(true)).unwrap();
        let msgs = drain_mpsc_receiver(&mut from).expect("mpsc disconnected unexpectedly");

        // Expect exactly one message with a value of "true" to arrive.
        assert_eq!(1, msgs.len());
        assert_eq!(true, msgs[0].load(Ordering::Relaxed));
    }

    #[test]
    fn drain_mpsc_receiver_disconnected() {
        let (mut to, mut from) = mpsc::channel();
        to.send(AtomicBool::new(true)).unwrap();
        drop(to);

        // Even though we sent something, if the channel has been disconnected, we don't want to
        // process any further.
        assert!(drain_mpsc_receiver(&mut from).is_err());
    }

    // UDP tests
    #[test]
    fn maybe_receive_from_no_data_test() {
        let mut socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .unwrap();
        let actual = maybe_receive_from_udp(&socket).unwrap();
        assert_eq!(None, actual);
    }

    #[test]
    fn maybe_receive_from_udp_test() {
        let buf = vec![0, 1, 2, 3, 4];
        let mut socket_a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut socket_b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket_b
            .send_to(&buf, socket_a.local_addr().unwrap())
            .unwrap();
        let actual = maybe_receive_from_udp(&socket_a).unwrap();
        assert_eq!(Some((buf, socket_b.local_addr().unwrap())), actual);
    }

    #[test]
    fn maybe_receive_no_data_test() {
        let mut socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .unwrap();
        let actual = maybe_receive_udp(&socket).unwrap();
        assert_eq!(Vec::<u8>::new(), actual);
    }

    #[test]
    fn maybe_receive_udp_test() {
        let buf = vec![0, 1, 2, 3, 4];
        let mut socket_a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut socket_b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket_b
            .send_to(&buf, socket_a.local_addr().unwrap())
            .unwrap();
        let actual = maybe_receive_udp(&socket_a).unwrap();
        assert_eq!(buf, actual);
    }
}
