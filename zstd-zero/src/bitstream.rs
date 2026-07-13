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
        let mut bit = 0usize;
        while bit < count {
            let source = start + bit;
            value |= (((self.data[source / 8] >> (source % 8)) & 1) as u32) << bit;
            bit += 1;
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
        let mut bit = 0usize;
        while bit < count {
            let source = self.position + bit;
            value |= (((self.data[source / 8] >> (source % 8)) & 1) as u32) << bit;
            bit += 1;
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
