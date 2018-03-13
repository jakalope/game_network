use std;
use std::collections::VecDeque;

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
