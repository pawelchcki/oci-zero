const PRIME1: u64 = 11_400_714_785_074_694_791;
const PRIME2: u64 = 14_029_467_366_897_019_727;
const PRIME3: u64 = 1_609_587_929_392_839_161;
const PRIME4: u64 = 9_650_029_242_287_828_579;
const PRIME5: u64 = 2_870_177_450_012_600_261;

#[derive(Clone, Copy)]
pub(crate) struct XxHash64 {
    lanes: [u64; 4],
    tail: [u8; 32],
    tail_len: usize,
    total_len: u64,
}

impl XxHash64 {
    pub(crate) const fn new() -> Self {
        Self {
            lanes: [
                PRIME1.wrapping_add(PRIME2),
                PRIME2,
                0,
                0u64.wrapping_sub(PRIME1),
            ],
            tail: [0; 32],
            tail_len: 0,
            total_len: 0,
        }
    }

    pub(crate) fn update(&mut self, mut input: &[u8]) {
        self.total_len = self.total_len.wrapping_add(input.len() as u64);
        if self.tail_len + input.len() < 32 {
            self.tail[self.tail_len..self.tail_len + input.len()].copy_from_slice(input);
            self.tail_len += input.len();
            return;
        }

        if self.tail_len != 0 {
            let needed = 32 - self.tail_len;
            self.tail[self.tail_len..].copy_from_slice(&input[..needed]);
            let block = self.tail;
            self.process(&block);
            input = &input[needed..];
            self.tail_len = 0;
        }

        while input.len() >= 32 {
            self.process(&input[..32]);
            input = &input[32..];
        }
        self.tail[..input.len()].copy_from_slice(input);
        self.tail_len = input.len();
    }

    pub(crate) fn digest(&self) -> u64 {
        let mut hash = if self.total_len >= 32 {
            let mut value = self.lanes[0]
                .rotate_left(1)
                .wrapping_add(self.lanes[1].rotate_left(7))
                .wrapping_add(self.lanes[2].rotate_left(12))
                .wrapping_add(self.lanes[3].rotate_left(18));
            for lane in self.lanes {
                value ^= round(0, lane);
                value = value.wrapping_mul(PRIME1).wrapping_add(PRIME4);
            }
            value
        } else {
            PRIME5
        };
        hash = hash.wrapping_add(self.total_len);

        let mut tail = &self.tail[..self.tail_len];
        while tail.len() >= 8 {
            let lane = round(0, read_u64(tail));
            hash ^= lane;
            hash = hash
                .rotate_left(27)
                .wrapping_mul(PRIME1)
                .wrapping_add(PRIME4);
            tail = &tail[8..];
        }
        if tail.len() >= 4 {
            hash ^= (read_u32(tail) as u64).wrapping_mul(PRIME1);
            hash = hash
                .rotate_left(23)
                .wrapping_mul(PRIME2)
                .wrapping_add(PRIME3);
            tail = &tail[4..];
        }
        for byte in tail {
            hash ^= (*byte as u64).wrapping_mul(PRIME5);
            hash = hash.rotate_left(11).wrapping_mul(PRIME1);
        }
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(PRIME2);
        hash ^= hash >> 29;
        hash = hash.wrapping_mul(PRIME3);
        hash ^ (hash >> 32)
    }

    fn process(&mut self, block: &[u8]) {
        for lane in 0..4 {
            self.lanes[lane] = round(self.lanes[lane], read_u64(&block[lane * 8..]));
        }
    }
}

fn round(accumulator: u64, input: u64) -> u64 {
    accumulator
        .wrapping_add(input.wrapping_mul(PRIME2))
        .rotate_left(31)
        .wrapping_mul(PRIME1)
}

fn read_u32(input: &[u8]) -> u32 {
    u32::from_le_bytes([input[0], input[1], input[2], input[3]])
}

fn read_u64(input: &[u8]) -> u64 {
    u64::from_le_bytes([
        input[0], input[1], input[2], input[3], input[4], input[5], input[6], input[7],
    ])
}

#[cfg(test)]
mod tests {
    use super::XxHash64;

    #[test]
    fn known_vectors() {
        let mut hash = XxHash64::new();
        assert_eq!(hash.digest(), 0xef46_db37_51d8_e999);
        hash.update(b"hello");
        assert_eq!(hash.digest(), 0x26c7_827d_889f_6da3);
    }

    #[test]
    fn long_vector_is_independent_of_update_boundaries() {
        let input = b"Nobody inspects the spammish repetition";
        let expected = 0xfbce_a83c_8a37_8bf1;

        for split in 0..=input.len() {
            let mut hash = XxHash64::new();
            hash.update(&input[..split]);
            hash.update(&input[split..]);
            assert_eq!(hash.digest(), expected, "split at byte {split}");
        }

        let mut hash = XxHash64::new();
        for byte in input {
            hash.update(core::slice::from_ref(byte));
        }
        assert_eq!(hash.digest(), expected);
    }

    #[test]
    fn hashes_an_exact_block_and_a_wide_tail() {
        let mut input = [0; 48];
        for (index, byte) in input.iter_mut().enumerate() {
            *byte = index as u8;
        }

        let mut hash = XxHash64::new();
        hash.update(&input[..32]);
        assert_eq!(hash.digest(), 0xcbf5_9c51_16ff_32b4);

        hash.update(&input[32..]);
        assert_eq!(hash.digest(), 0x8fe4_3763_2da0_6964);
    }
}
