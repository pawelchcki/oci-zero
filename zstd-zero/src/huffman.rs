use crate::bitstream::BackwardBits;
use crate::fse::Table as FseTable;
use crate::DecodeError;

const MAX_BITS: u8 = 11;
const TABLE_SIZE: usize = 1 << MAX_BITS;

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    symbol: u8,
    bits: u8,
}

pub(crate) struct Table {
    entries: [Entry; TABLE_SIZE],
    table_bits: u8,
    valid: bool,
}

impl Table {
    pub(crate) const fn new() -> Self {
        Self {
            entries: [Entry { symbol: 0, bits: 0 }; TABLE_SIZE],
            table_bits: 0,
            valid: false,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.valid
    }

    pub(crate) fn read_description(&mut self, input: &[u8]) -> Result<usize, DecodeError> {
        let header = *input.first().ok_or(DecodeError::InvalidEntropyTable)?;
        let mut weights = [0u8; 256];
        let (weight_count, consumed) = if header < 128 {
            let compressed_size = header as usize;
            if compressed_size == 0 || input.len() < 1 + compressed_size {
                return Err(DecodeError::InvalidEntropyTable);
            }
            let count = decode_compressed_weights(&input[1..1 + compressed_size], &mut weights)?;
            (count, 1 + compressed_size)
        } else {
            let count = header as usize - 127;
            let bytes = count.div_ceil(2);
            if input.len() < 1 + bytes {
                return Err(DecodeError::InvalidEntropyTable);
            }
            for (index, weight) in weights[..count].iter_mut().enumerate() {
                let packed = input[1 + index / 2];
                *weight = if index % 2 == 0 {
                    packed >> 4
                } else {
                    packed & 0x0f
                };
            }
            (count, 1 + bytes)
        };
        self.build(&weights[..weight_count])?;
        Ok(consumed)
    }

    pub(crate) fn decode(&self, input: &[u8], output: &mut [u8]) -> Result<(), DecodeError> {
        if !self.valid {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let mut bits = BackwardBits::new(input)?;
        for byte in output {
            let index = bits.peek_padded(self.table_bits) as usize;
            let entry = self.entries[index];
            if entry.bits == 0 {
                return Err(DecodeError::InvalidEntropyTable);
            }
            let consumed = core::cmp::min(entry.bits as usize, bits.remaining()) as u8;
            bits.consume(consumed)?;
            *byte = entry.symbol;
        }
        if bits.remaining() != 0 {
            return Err(DecodeError::InvalidBitstream);
        }
        Ok(())
    }

    pub(crate) fn decode_four(&self, input: &[u8], output: &mut [u8]) -> Result<(), DecodeError> {
        if input.len() < 6 {
            return Err(DecodeError::InvalidBitstream);
        }
        let sizes = [
            u16::from_le_bytes([input[0], input[1]]) as usize,
            u16::from_le_bytes([input[2], input[3]]) as usize,
            u16::from_le_bytes([input[4], input[5]]) as usize,
        ];
        let starts = [
            6,
            6usize
                .checked_add(sizes[0])
                .ok_or(DecodeError::ArithmeticOverflow)?,
            6usize
                .checked_add(sizes[0])
                .and_then(|value| value.checked_add(sizes[1]))
                .ok_or(DecodeError::ArithmeticOverflow)?,
            6usize
                .checked_add(sizes[0])
                .and_then(|value| value.checked_add(sizes[1]))
                .and_then(|value| value.checked_add(sizes[2]))
                .ok_or(DecodeError::ArithmeticOverflow)?,
        ];
        if starts[3] > input.len() {
            return Err(DecodeError::InvalidBitstream);
        }
        let ends = [starts[1], starts[2], starts[3], input.len()];
        let segment = output.len().div_ceil(4);
        for stream in 0..4 {
            let output_start = core::cmp::min(stream * segment, output.len());
            let output_end = core::cmp::min(output_start + segment, output.len());
            if output_start != output_end {
                self.decode(
                    &input[starts[stream]..ends[stream]],
                    &mut output[output_start..output_end],
                )?;
            }
        }
        Ok(())
    }

    fn build(&mut self, weights: &[u8]) -> Result<(), DecodeError> {
        if weights.is_empty() || weights.len() >= 256 {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let mut sum = 0u32;
        for weight in weights {
            if *weight > MAX_BITS {
                return Err(DecodeError::InvalidEntropyTable);
            }
            if *weight != 0 {
                sum = sum
                    .checked_add(1u32 << (*weight - 1))
                    .ok_or(DecodeError::InvalidEntropyTable)?;
            }
        }
        if sum == 0 {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let table_bits = 32 - sum.leading_zeros();
        if table_bits > MAX_BITS as u32 {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let remainder = (1u32 << table_bits)
            .checked_sub(sum)
            .ok_or(DecodeError::InvalidEntropyTable)?;
        if !remainder.is_power_of_two() {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let implied_weight = 32 - remainder.leading_zeros();
        let table_bits = table_bits as u8;

        let mut code_bits = [0u8; 256];
        for (index, weight) in weights.iter().enumerate() {
            code_bits[index] = if *weight == 0 {
                0
            } else {
                table_bits + 1 - *weight
            };
        }
        code_bits[weights.len()] = table_bits + 1 - implied_weight as u8;

        let mut ranks = [0usize; MAX_BITS as usize + 1];
        for bits in &code_bits[..=weights.len()] {
            ranks[*bits as usize] += 1;
        }
        let mut starts = [0usize; MAX_BITS as usize + 1];
        for bits in (1..=table_bits as usize).rev() {
            starts[bits - 1] =
                starts[bits] + ranks[bits] * (1usize << (table_bits as usize - bits));
        }
        let used = 1usize << table_bits;
        if starts[0] != used {
            return Err(DecodeError::InvalidEntropyTable);
        }
        self.entries[..used].fill(Entry::default());
        for (symbol, bits) in code_bits[..=weights.len()].iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            let width = 1usize << (table_bits - *bits);
            let start = starts[*bits as usize];
            let end = start
                .checked_add(width)
                .ok_or(DecodeError::InvalidEntropyTable)?;
            if end > used {
                return Err(DecodeError::InvalidEntropyTable);
            }
            self.entries[start..end].fill(Entry {
                symbol: symbol as u8,
                bits: *bits,
            });
            starts[*bits as usize] = end;
        }
        self.table_bits = table_bits;
        self.valid = true;
        Ok(())
    }
}

fn decode_compressed_weights(input: &[u8], output: &mut [u8; 256]) -> Result<usize, DecodeError> {
    let mut table = FseTable::new();
    let description_size = table.read_description(input, 12, 6)?;
    if description_size >= input.len() {
        return Err(DecodeError::InvalidEntropyTable);
    }
    let mut bits = BackwardBits::new(&input[description_size..])?;
    let mut first = bits.read(table.log())?;
    let mut second = bits.read(table.log())?;
    let mut count = 0usize;
    loop {
        if count >= 255 {
            return Err(DecodeError::InvalidEntropyTable);
        }
        output[count] = table.symbol(first)?;
        count += 1;
        let entry = table.entry(first)?;
        if bits.remaining() < entry.bits as usize {
            if count >= 255 {
                return Err(DecodeError::InvalidEntropyTable);
            }
            output[count] = table.symbol(second)?;
            return Ok(count + 1);
        }
        table.update(&mut first, &mut bits)?;

        if count >= 255 {
            return Err(DecodeError::InvalidEntropyTable);
        }
        output[count] = table.symbol(second)?;
        count += 1;
        let entry = table.entry(second)?;
        if bits.remaining() < entry.bits as usize {
            if count >= 255 {
                return Err(DecodeError::InvalidEntropyTable);
            }
            output[count] = table.symbol(first)?;
            return Ok(count + 1);
        }
        table.update(&mut second, &mut bits)?;
    }
}
