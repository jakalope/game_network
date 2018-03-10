extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate bincode;

use std::sync::mpsc;
use std::net::{SocketAddr, UdpSocket};
use std::collections::{HashMap, VecDeque};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, Eq)]
pub struct Credentials {
    username: String,
}

#[derive(Serialize, Deserialize, Copy, Clone, PartialEq, Debug, Hash, Eq)]
pub struct ClientId(usize);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BitVec {
    storage: Vec<u32>, // Storage space for bits.
    len_of_last: u8, // Bits in use in the last element of storage.
}

/// Represents a compressed `VecDeque<BitVec>` where each `BitVec` has the same length.
/// This data structure is used to implement the networking strategy described by
/// https://gafferongames.com/post/deterministic_lockstep/
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct CompressedBitVec {
    // Compressed vector of BitVecs.
    bits: BitVec,

    // Number of bits in each of the original BitVecs before they were compressed.
    element_size: usize,
}

pub enum SendError {
    FailedToCompress,
    FailedToSerialize(Box<bincode::ErrorKind>),
    FailedToSend(std::io::Error),
}

pub enum RecvError {
    FailedToReceive(std::io::Error),
    FailedToDeserialize(Box<bincode::ErrorKind>),
    FailedToDecompress,
    FailedToSerializeAck(Box<bincode::ErrorKind>),
    FailedToSendAck(std::io::Error),
}

pub struct ControllerSequence {
    start_tick: usize, // Game tick of first element in seq.
    seq: VecDeque<BitVec>, // Sequence of game controller states.
    client_id: ClientId, // Identifies the client associated with this control sequence.
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CompressedControllerSequence {
    start_tick: usize,
    comp_seq: CompressedBitVec,
    client_id: ClientId,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
enum ClientMessage {
    JoinRequest(Credentials),
    ControllerInput(CompressedControllerSequence),
}

#[derive(Serialize, Deserialize, Clone)]
enum ServerMessage<StateT>
where
    StateT: serde::Serialize,
{
    JoinConfirmation,
    LastTickReceived(usize),
    WorldState(StateT),
}

pub struct Client<StateT> {
    socket: UdpSocket,
    server_addr: SocketAddr,
    self_addr: SocketAddr,
    controller_seq: ControllerSequence,
    state_sender: mpsc::Sender<StateT>,
}

pub struct Server {
    socket: UdpSocket,
    self_addr: SocketAddr,
    client_inputs: HashMap<ClientId, ControllerSequence>,
}

impl BitVec {
    pub fn new() -> Self {
        BitVec {
            storage: vec![0u32],
            len_of_last: 0u8,
        }
    }

    pub fn from_slice(slice: &[bool]) -> Self {
        let mut bit_vec = BitVec::new();
        for bit in slice {
            bit_vec.push(*bit);
        }
        bit_vec
    }

    pub fn len(&self) -> usize {
        32usize * (self.storage.len() - 1) + (self.len_of_last as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.len_of_last == 0
    }

    pub fn push(&mut self, bit: bool) {
        if self.len_of_last == 32 {
            // We're at the end of the current u32, we need to add a new one and set the first bit.
            self.storage.push(bit as u32);
            self.len_of_last = 1;
        } else if bit == true {
            // Explicitly set a one-bit in the next available position.
            // This is safe as long as len_of_last is always between 0 and 31.
            let new_bit = 0x01u32.checked_shl(self.len_of_last as u32).unwrap();
            // Unwrap is safe here as long as we maintain at least 1 element of storage.
            *self.storage.last_mut().unwrap() = self.storage.last().unwrap() | new_bit;
            self.len_of_last += 1;
        } else {
            // No need to explicitly set a bit. Just add a zero by incrementing the length.
            self.len_of_last += 1;
        }
    }

    pub fn append(&mut self, other: &BitVec) {
        for bit in other {
            self.push(bit);
        }
    }

    pub fn range(&self, range: std::ops::Range<usize>) -> Option<Self> {
        let mut bitvec = BitVec::new();
        for idx in range {
            let bit = self.get(idx)?;
            bitvec.push(bit);
        }
        Some(bitvec)
    }

    pub fn get(&self, index: usize) -> Option<bool> {
        if index >= self.len() {
            return None;
        }

        let outer_idx = index / 32;
        let element = self.storage.get(outer_idx)?;
        let inner_idx = (index % 32) as u32;
        let bit = 0x01u32.checked_shl(inner_idx).unwrap();

        Some((*element & bit) != 0)
    }
}

pub struct BitVecIter<'a> {
    bitvec: &'a BitVec, // Object being iterated over.
    bit_counter: usize, // Current bit pointed to by the iterator.
}

impl<'a> std::iter::Iterator for BitVecIter<'a> {
    type Item = bool;
    fn next(&mut self) -> Option<Self::Item> {
        match self.bitvec.get(self.bit_counter) {
            Some(b) => {
                self.bit_counter += 1;
                return Some(b);
            }
            None => None,
        }
    }
}

impl<'a> std::iter::IntoIterator for &'a BitVec {
    type Item = bool;
    type IntoIter = BitVecIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        BitVecIter {
            bitvec: &self,
            bit_counter: 0,
        }
    }
}

fn decompress(element_size: usize, payload: &BitVec) -> Option<VecDeque<BitVec>> {
    let mut seq = VecDeque::<BitVec>::new();
    let mut idx = 0usize;
    let mut previous: Option<BitVec> = None;
    while idx < payload.len() {
        let bit = payload.get(idx)?;
        match bit {
            true => {
                let range = std::ops::Range::<usize> {
                    start: idx + 1,
                    end: idx + element_size + 1,
                };
                let previous_element = payload.range(range)?;
                seq.push_back(previous_element.clone());
                previous = Some(previous_element);
                idx += element_size + 1;
            }
            false => {
                if let Some(element) = previous.clone() {
                    seq.push_back(element);
                    idx += 1;
                } else {
                    return None;
                }
            }
        }
    }

    Some(seq)
}

fn compress(element_size: usize, seq: &VecDeque<BitVec>) -> Option<BitVec> {
    // For each contiguous, equal element, add a zero-bit. For each contiguous element that
    // isn't equal to the previous, add a one-bit followed by the new value.
    let mut previous: Option<&BitVec> = None;
    let mut payload = BitVec::new();
    for element in seq {
        if element.len() != element_size {
            // Only equal-length BitVecs are supported.
            return None;
        }
        if previous.is_none() || *element != *previous.unwrap() {
            // Store non-repetitive elements directly.
            payload.push(true);
            payload.append(element);
            previous = Some(element);
        } else if *element == *previous.unwrap() {
            // Compress repetitive elements as a single zero-bit.
            payload.push(false);
        }
    }

    Some(payload)
}

impl CompressedBitVec {
    /// Creates a compressed representation of a series of equally sized `BitVec`s.
    /// If `seq` are not equally sized, returns `None`.
    pub fn compress(seq: &VecDeque<BitVec>) -> Option<Self> {
        let element_size = seq.front().map_or(0, |element| element.len());
        let bits = compress(element_size, seq);
        Some(CompressedBitVec {
            bits: bits?,
            element_size: element_size,
        })
    }

    /// Decompresses `self` into a `VecDeque` of equally sized `BitVec`s.
    pub fn decompress(&self) -> Option<VecDeque<BitVec>> {
        decompress(self.element_size, &self.bits)
    }
}



impl ControllerSequence {
    pub fn push(&mut self, bit_vec: BitVec) {
        self.seq.push_back(bit_vec);
    }

    pub fn to_compressed(&self) -> Option<CompressedControllerSequence> {
        let comp_seq = CompressedBitVec::compress(&self.seq)?;
        Some(CompressedControllerSequence {
            start_tick: self.start_tick,
            comp_seq: comp_seq,
            client_id: self.client_id,
        })
    }

    pub fn last_tick(&self) -> usize {
        self.start_tick + self.seq.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seq.is_empty()
    }

    pub fn remove_till_tick(&mut self, tick: usize) {
        let last_tick = self.last_tick();
        if last_tick > tick {
            let tick_count = last_tick - tick;
            let end = std::cmp::min(tick_count, self.seq.len());
            self.seq.drain(0..end);
        }
    }
}


impl CompressedControllerSequence {
    pub fn to_controller_sequence(self) -> Option<ControllerSequence> {
        let seq = self.comp_seq.decompress()?;
        Some(ControllerSequence {
            start_tick: self.start_tick,
            seq: seq,
            client_id: self.client_id,
        })
    }
}

impl<StateT> Client<StateT> {
    fn connect(&mut self) -> std::io::Result<()> {
        self.socket = UdpSocket::bind(self.self_addr)?;
        self.socket.connect(self.server_addr)
    }

    pub fn send(&self) -> Result<(), SendError> {
        if !self.controller_seq.is_empty() {
            let payload = self.controller_seq.to_compressed().ok_or(
                SendError::FailedToCompress,
            )?;

            let controller_input = ClientMessage::ControllerInput(payload);

            let encoded: Vec<u8> = bincode::serialize(&controller_input).map_err(|err| {
                SendError::FailedToSerialize(err)
            })?;

            self.socket.send_to(&encoded, self.server_addr).map_err(
                |err| {
                    SendError::FailedToSend(err)
                },
            )?;
        }

        Ok(())
    }

    fn handle_last_tick_received(&mut self, last_tick: usize) -> Result<(), RecvError> {
        // Now we remove the controller inputs the server has ACK'd.
        self.controller_seq.remove_till_tick(last_tick + 1);
        Ok(())
    }

    fn handle_join_confirmation(&self) -> Result<(), RecvError> {
        Ok(())
    }

    fn handle_world_state<'de>(&mut self, state: StateT) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        Ok(())
    }

    pub fn receive(&mut self) -> Result<(), RecvError>
    where
        StateT: serde::de::DeserializeOwned,
        StateT: serde::Serialize,
    {
        // Receive ACK only from the connected server.
        let mut buf = Vec::<u8>::new();
        self.socket.recv(&mut buf).map_err(|err| {
            RecvError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        // The ACK contains the latest controller input the server has received from us.
        let server_message: ServerMessage<StateT> = bincode::deserialize(&buf[..]).map_err(|err| {
            RecvError::FailedToDeserialize(err)
        })?;

        match server_message {
            ServerMessage::LastTickReceived(tick) => self.handle_last_tick_received(tick),
            ServerMessage::JoinConfirmation => self.handle_join_confirmation(),
            ServerMessage::WorldState(state) => self.handle_world_state(state),
        }
    }
}

impl<'de> Server {
    fn bind(&mut self) -> std::io::Result<()> {
        self.socket = UdpSocket::bind(self.self_addr)?;
        Ok(())
    }

    fn handle_controller<StateT>(
        &mut self,
        input: CompressedControllerSequence,
        src: SocketAddr,
    ) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {
        // Decompress the deserialized controller input sequence.
        let controller_seq = input.to_controller_sequence().ok_or(
            RecvError::FailedToDecompress,
        )?;

        // Compute game tick of last controller input received.
        let last_tick: ServerMessage<StateT> =
            ServerMessage::LastTickReceived(controller_seq.start_tick + controller_seq.seq.len());

        // Add or update the inputs in the client inputs map.
        self.client_inputs.insert(
            controller_seq.client_id,
            controller_seq,
        );

        // Serialize the tick.
        let encode = bincode::serialize(&last_tick).map_err(|err| {
            RecvError::FailedToSerializeAck(err)
        })?;

        // Send the tick.
        self.socket.send_to(&encode, src).map_err(|err| {
            RecvError::FailedToSendAck(err)
        })?;

        Ok(())
    }

    fn handle_join_request<StateT>(
        &mut self,
        cred: Credentials,
        src: SocketAddr,
    ) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {

        // TODO

        Ok(())
    }

    pub fn receive<StateT>(&mut self) -> Result<(), RecvError>
    where
        StateT: serde::Deserialize<'de>,
        StateT: serde::Serialize,
    {

        // Receive inputs from anyone.
        let mut buf = Vec::<u8>::new();
        let (_, src) = self.socket.recv_from(&mut buf).map_err(|err| {
            RecvError::FailedToReceive(err)
        })?;

        // Deserialize the received datagram.
        let client_message: ClientMessage = bincode::deserialize(&buf).map_err(|err| {
            RecvError::FailedToDeserialize(err)
        })?;

        match client_message {
            ClientMessage::ControllerInput(input) => self.handle_controller::<StateT>(input, src),
            ClientMessage::JoinRequest(cred) => self.handle_join_request::<StateT>(cred, src),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn bitvec_empty() {
        // Tests the requirement that all bitvecs must have at least one storage element.
        let bitvec = BitVec::new();
        assert_eq!(0, bitvec.len());
        assert!(bitvec.is_empty());
    }

    #[test]
    fn bitvec_not_empty() {
        let mut bitvec = BitVec::new();
        bitvec.push(true);
        assert_eq!(1, bitvec.len());
        assert_eq!(false, bitvec.is_empty());
    }

    #[test]
    fn bitvec_gt_32() {
        let mut bitvec = BitVec::new();
        for i in 0..33 {
            bitvec.push((i % 2) == 1);
        }
        for i in 0..33 {
            assert_eq!(Some((i % 2) == 1), bitvec.get(i));
        }
        assert_eq!(33, bitvec.len());
        assert_eq!(None, bitvec.get(33));
    }

    #[test]
    fn bitvec_push_get() {
        let mut bitvec = BitVec::new();
        assert_eq!(None, bitvec.get(0));
        assert_eq!(None, bitvec.get(1));

        bitvec.push(true);
        assert_eq!(Some(true), bitvec.get(0));
        assert_eq!(None, bitvec.get(1));

        bitvec.push(false);
        assert_eq!(Some(true), bitvec.get(0));
        assert_eq!(Some(false), bitvec.get(1));
        assert_eq!(None, bitvec.get(2));
    }

    #[test]
    fn bitvec_range() {
        let first = BitVec::from_slice(&[false, true, false]);
        assert_eq!(first, first.range(0..3).unwrap());
        assert_eq!(None, first.range(0..4));
    }

    #[test]
    fn bitvec_append() {
        let first = BitVec::from_slice(&[false, true, false]);
        let mut second = BitVec::new();
        second.push(true);
        second.append(&first);
        assert_eq!(first, second.range(1..4).unwrap());
        assert!(first != second.range(0..3).unwrap());
        assert!(first != second.range(1..3).unwrap());
    }

    #[test]
    fn compress() {
        let bits = BitVec::from_slice(&[true, true, false]);
        let vec = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let comp = super::compress(3, &vec).unwrap();
        let expected = BitVec::from_slice(&[true, true, true, false, false]);
        assert_eq!(expected, comp);
    }

    #[test]
    fn decompress() {
        let comp = BitVec::from_slice(&[true, true, true, false, false]);
        let bits = BitVec::from_slice(&[true, true, false]);
        let expected = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let decomp = super::decompress(3, &comp).unwrap();
        assert_eq!(expected, decomp);
    }

    #[test]
    fn round_trip() {
        let bits = BitVec::from_slice(&[true, true, false]);
        let expected = VecDeque::from(vec![bits.clone(), bits.clone()]);
        let obj = CompressedBitVec::compress(&expected).unwrap();
        let decomp = obj.decompress().unwrap();
        assert_eq!(expected, decomp);
    }
}
