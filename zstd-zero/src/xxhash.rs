//! Zstandard uses streaming XXH64 with seed zero for its frame checksum.

pub(crate) use xxhash_rust::xxh64::Xxh64 as XxHash64;

#[cfg(test)]
mod tests {
    use super::XxHash64;

    #[test]
    fn known_vectors() {
        let mut hash = XxHash64::new(0);
        assert_eq!(hash.digest(), 0xef46_db37_51d8_e999);
        hash.update(b"hello");
        assert_eq!(hash.digest(), 0x26c7_827d_889f_6da3);
    }

    #[test]
    fn long_vector_is_independent_of_update_boundaries() {
        let input = b"Nobody inspects the spammish repetition";
        let expected = 0xfbce_a83c_8a37_8bf1;

        for split in 0..=input.len() {
            let mut hash = XxHash64::new(0);
            hash.update(&input[..split]);
            hash.update(&input[split..]);
            assert_eq!(hash.digest(), expected, "split at byte {split}");
        }

        let mut hash = XxHash64::new(0);
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

        let mut hash = XxHash64::new(0);
        hash.update(&input[..32]);
        assert_eq!(hash.digest(), 0xcbf5_9c51_16ff_32b4);

        hash.update(&input[32..]);
        assert_eq!(hash.digest(), 0x8fe4_3763_2da0_6964);
    }
}
