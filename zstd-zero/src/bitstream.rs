use crate::DecodeError;

pub(crate) struct BackwardBits<'a> {
    data: &'a [u8],
    remaining: usize,
}

impl<'a> BackwardBits<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Result<Self, DecodeError> {
        let last = *data.last().ok_or(DecodeError::InvalidBitstream)?;
        if last == 0 {
            return Err(DecodeError::InvalidBitstream);
        }
        let marker = 7 - last.leading_zeros() as usize;
        Ok(Self {
            data,
            remaining: (data.len() - 1) * 8 + marker,
        })
    }

    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }

    pub(crate) fn read(&mut self, count: u8) -> Result<u32, DecodeError> {
        let count = count as usize;
        if count > 32 || count > self.remaining {
            return Err(DecodeError::InvalidBitstream);
        }
        self.remaining -= count;
        Ok(self.extract(self.remaining, count))
    }

    pub(crate) fn peek_padded(&self, count: u8) -> u32 {
        let count = count as usize;
        let available = core::cmp::min(count, self.remaining);
        let value = self.extract(self.remaining - available, available);
        value << (count - available)
    }

    pub(crate) fn consume(&mut self, count: u8) -> Result<(), DecodeError> {
        let count = count as usize;
        if count > self.remaining {
            return Err(DecodeError::InvalidBitstream);
        }
        self.remaining -= count;
        Ok(())
    }

    fn extract(&self, start: usize, count: usize) -> u32 {
        let mut value = 0u32;
        for bit in 0..count {
            let source = start + bit;
            value |= (((self.data[source / 8] >> (source % 8)) & 1) as u32) << bit;
        }
        value
    }
}

pub(crate) struct ForwardBits<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ForwardBits<'a> {
    pub(crate) fn new(data: &'a [u8], position: usize) -> Self {
        Self { data, position }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn read(&mut self, count: u8) -> Result<u32, DecodeError> {
        let count = count as usize;
        if count > 32 || self.position + count > self.data.len() * 8 {
            return Err(DecodeError::InvalidBitstream);
        }
        let mut value = 0u32;
        for bit in 0..count {
            let source = self.position + bit;
            value |= (((self.data[source / 8] >> (source % 8)) & 1) as u32) << bit;
        }
        self.position += count;
        Ok(value)
    }

    pub(crate) fn rewind(&mut self, count: usize) -> Result<(), DecodeError> {
        if count > self.position {
            return Err(DecodeError::InvalidBitstream);
        }
        self.position -= count;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BackwardBits, ForwardBits};
    use crate::DecodeError;

    fn bits(data: &[u8], start: usize, count: usize) -> u32 {
        let mut value = 0;
        for bit in 0..count {
            value |= u32::from((data[(start + bit) / 8] >> ((start + bit) % 8)) & 1) << bit;
        }
        value
    }

    #[test]
    fn forward_reads_every_position_and_width() {
        let data = [0xa6, 0x39, 0xc5, 0x5a, 0x87, 0xe1];
        let bit_length = data.len() * 8;

        for start in 0..=bit_length {
            for count in 0..=core::cmp::min(32, bit_length - start) {
                let mut reader = ForwardBits::new(&data, start);
                assert_eq!(reader.position(), start);
                assert_eq!(reader.read(count as u8), Ok(bits(&data, start, count)));
                assert_eq!(reader.position(), start + count);
            }
        }

        assert_eq!(
            ForwardBits::new(&data, 0).read(33),
            Err(DecodeError::InvalidBitstream)
        );
        assert_eq!(
            ForwardBits::new(&data, bit_length - 4).read(5),
            Err(DecodeError::InvalidBitstream)
        );
    }

    #[test]
    fn forward_rewinds_with_exact_boundaries() {
        let mut reader = ForwardBits::new(&[0xff; 3], 17);
        assert_eq!(reader.rewind(5), Ok(()));
        assert_eq!(reader.position(), 12);
        assert_eq!(reader.rewind(12), Ok(()));
        assert_eq!(reader.position(), 0);
        assert_eq!(reader.rewind(1), Err(DecodeError::InvalidBitstream));
    }

    #[test]
    fn backward_reads_and_peeks_every_width() {
        let data = [0xa6, 0x39, 0b0001_0101];
        let bit_length = 20;
        let reader = BackwardBits::new(&data).unwrap();
        assert_eq!(reader.remaining(), bit_length);

        for count in 0..=bit_length {
            let mut reader = BackwardBits::new(&data).unwrap();
            assert_eq!(
                reader.read(count as u8),
                Ok(bits(&data, bit_length - count, count))
            );
            assert_eq!(reader.remaining(), bit_length - count);
        }

        for count in 0..=32 {
            let available = core::cmp::min(count, bit_length);
            let expected = bits(&data, bit_length - available, available) << (count - available);
            assert_eq!(reader.peek_padded(count as u8), expected, "width {count}");
        }

        let mut reader = BackwardBits::new(&data).unwrap();
        for count in [3, 8, 5, 4] {
            let remaining = reader.remaining();
            assert_eq!(
                reader.read(count),
                Ok(bits(&data, remaining - count as usize, count as usize))
            );
        }
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.read(1), Err(DecodeError::InvalidBitstream));
    }

    #[test]
    fn backward_validates_widths_and_consumption() {
        assert!(matches!(
            BackwardBits::new(&[]),
            Err(DecodeError::InvalidBitstream)
        ));
        assert!(matches!(
            BackwardBits::new(&[0]),
            Err(DecodeError::InvalidBitstream)
        ));

        let wide = [0xa6, 0x39, 0xc5, 0x5a, 0x87, 0x80];
        let mut reader = BackwardBits::new(&wide).unwrap();
        assert_eq!(reader.read(32), Ok(bits(&wide, 15, 32)));
        assert_eq!(
            BackwardBits::new(&wide).unwrap().read(33),
            Err(DecodeError::InvalidBitstream)
        );

        let mut reader = BackwardBits::new(&[0xa6, 0x39, 0b0001_0101]).unwrap();
        assert_eq!(reader.consume(7), Ok(()));
        assert_eq!(reader.remaining(), 13);
        assert_eq!(reader.consume(13), Ok(()));
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.consume(1), Err(DecodeError::InvalidBitstream));
    }
}
