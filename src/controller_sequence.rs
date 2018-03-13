use std;
use std::collections::VecDeque;
use bitvec::{BitVec, CompressedBitVec};

pub struct ControllerSequence {
    start_tick: usize, // Game tick of first element in seq.
    seq: VecDeque<BitVec>, // Sequence of game controller states.
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CompressedControllerSequence {
    start_tick: usize,
    comp_seq: CompressedBitVec,
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
        })
    }
}
