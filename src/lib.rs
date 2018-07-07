extern crate serde;
#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;
extern crate bincode;
extern crate bidir_map;
extern crate spmc;

#[macro_use]
mod msg;

mod bitvec;
mod control;
pub mod server;
pub mod client;

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
}
