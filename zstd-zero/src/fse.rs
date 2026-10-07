use crate::bitstream::{BackwardBits, ForwardBits};
use crate::DecodeError;

pub(crate) const MAX_TABLE_LOG: u8 = 9;
pub(crate) const MAX_SYMBOLS: usize = 256;

/// An initialized entropy-table slot; the decoder manages its contents.
#[derive(Clone, Copy, Debug, Default)]
pub struct Entry {
    pub(crate) baseline: u16,
    pub(crate) bits: u8,
    pub(crate) symbol: u8,
}

impl Entry {
    /// Create an empty slot, including for statically allocated workspaces.
    pub const fn new() -> Self {
        Self {
            baseline: 0,
            bits: 0,
            symbol: 0,
        }
    }
}

pub(crate) struct Table<'a> {
    entries: &'a mut [Entry],
    len: usize,
    log: u8,
    valid: bool,
}

impl<'a> Table<'a> {
    pub(crate) fn new(entries: &'a mut [Entry]) -> Self {
        Self {
            entries,
            len: 0,
            log: 0,
            valid: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.len = 0;
        self.log = 0;
        self.valid = false;
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.valid
    }

    pub(crate) fn log(&self) -> u8 {
        self.log
    }

    pub(crate) fn symbol(&self, state: u32) -> Result<u8, DecodeError> {
        self.entry(state).map(|entry| entry.symbol)
    }

    pub(crate) fn update(
        &self,
        state: &mut u32,
        bits: &mut BackwardBits<'_>,
    ) -> Result<(), DecodeError> {
        let entry = self.entry(*state)?;
        *state = entry.baseline as u32 + bits.read(entry.bits)?;
        Ok(())
    }

    pub(crate) fn entry(&self, state: u32) -> Result<Entry, DecodeError> {
        if !self.valid || state as usize >= self.len {
            return Err(DecodeError::InvalidEntropyTable);
        }
        Ok(self.entries[state as usize])
    }

    pub(crate) fn build_rle(&mut self, symbol: u8) {
        self.entries[0] = Entry {
            baseline: 0,
            bits: 0,
            symbol,
        };
        self.len = 1;
        self.log = 0;
        self.valid = true;
    }

    pub(crate) fn build(
        &mut self,
        probabilities: &[i16],
        table_log: u8,
        scratch: &mut [i16],
    ) -> Result<(), DecodeError> {
        let workspace = scratch
            .get_mut(..probabilities.len())
            .ok_or(DecodeError::InvalidEntropyTable)?;
        workspace.copy_from_slice(probabilities);
        self.build_in_place(workspace, table_log)
    }

    fn build_in_place(
        &mut self,
        probabilities: &mut [i16],
        table_log: u8,
    ) -> Result<(), DecodeError> {
        if table_log > MAX_TABLE_LOG
            || probabilities.is_empty()
            || probabilities.len() > MAX_SYMBOLS
        {
            return Err(DecodeError::InvalidEntropyTable);
        }
        if table_log == 0 {
            let symbol = probabilities
                .iter()
                .position(|probability| *probability == 1)
                .ok_or(DecodeError::InvalidEntropyTable)?;
            self.build_rle(symbol as u8);
            return Ok(());
        }

        let table_size = 1usize << table_log;
        let mut total = 0usize;
        for probability in probabilities.iter() {
            total = total
                .checked_add(if *probability == -1 {
                    1
                } else if *probability > 0 {
                    *probability as usize
                } else {
                    0
                })
                .ok_or(DecodeError::ArithmeticOverflow)?;
        }
        if total != table_size {
            return Err(DecodeError::InvalidEntropyTable);
        }

        self.entries[..table_size].fill(Entry::default());
        let mut high = table_size;
        for (symbol, probability) in probabilities.iter().enumerate() {
            if *probability == -1 {
                high = high
                    .checked_sub(1)
                    .ok_or(DecodeError::InvalidEntropyTable)?;
                self.entries[high].symbol = symbol as u8;
            }
        }

        let step = (table_size >> 1) + (table_size >> 3) + 3;
        let mask = table_size - 1;
        let mut position = 0usize;
        for (symbol, probability) in probabilities.iter().enumerate() {
            if *probability <= 0 {
                continue;
            }
            for _ in 0..*probability as usize {
                if position >= high {
                    return Err(DecodeError::InvalidEntropyTable);
                }
                self.entries[position].symbol = symbol as u8;
                position = (position + step) & mask;
                while position >= high {
                    position = (position + step) & mask;
                }
            }
        }
        if position != 0 {
            return Err(DecodeError::InvalidEntropyTable);
        }

        // Symbol spreading no longer needs the probabilities. Reuse their
        // storage for next-state counters (at most twice the 512-entry table).
        for probability in probabilities.iter_mut() {
            *probability = match *probability {
                -1 => 1,
                value if value > 0 => value,
                _ => 0,
            };
        }
        let next = probabilities;
        for entry in &mut self.entries[..table_size] {
            let symbol = entry.symbol as usize;
            let state =
                u16::try_from(next[symbol]).map_err(|_| DecodeError::InvalidEntropyTable)?;
            if state == 0 {
                return Err(DecodeError::InvalidEntropyTable);
            }
            next[symbol] =
                i16::try_from(state + 1).map_err(|_| DecodeError::InvalidEntropyTable)?;
            let floor_log = (u16::BITS - 1 - state.leading_zeros()) as u8;
            let bits = table_log - floor_log;
            entry.bits = bits;
            entry.baseline = ((state as u32) << bits)
                .checked_sub(table_size as u32)
                .ok_or(DecodeError::InvalidEntropyTable)? as u16;
        }

        self.len = table_size;
        self.log = table_log;
        self.valid = true;
        Ok(())
    }

    pub(crate) fn read_description(
        &mut self,
        input: &[u8],
        max_symbol: usize,
        max_log: u8,
        scratch: &mut [i16],
    ) -> Result<usize, DecodeError> {
        if input.is_empty() || max_symbol >= MAX_SYMBOLS {
            return Err(DecodeError::InvalidEntropyTable);
        }
        let mut bits = ForwardBits::new(input, 0);
        let table_log = bits.read(4)? as u8 + 5;
        if table_log > max_log || table_log > MAX_TABLE_LOG {
            return Err(DecodeError::InvalidEntropyTable);
        }

        let expected = 1u32 << table_log;
        let mut accumulated = 0u32;
        let probabilities = scratch
            .get_mut(..MAX_SYMBOLS)
            .ok_or(DecodeError::InvalidEntropyTable)?;
        // Zero-run symbols must not inherit counters from a previous table.
        probabilities.fill(0);
        let mut symbols = 0usize;
        while accumulated < expected {
            if symbols > max_symbol {
                return Err(DecodeError::InvalidEntropyTable);
            }
            let remaining = expected - accumulated + 1;
            let width = (32 - remaining.leading_zeros()) as u8;
            let unchecked = bits.read(width)?;
            let low_threshold = (1u32 << width) - 1 - remaining;
            let low_mask = (1u32 << (width - 1)) - 1;
            let low = unchecked & low_mask;
            let value = if low < low_threshold {
                bits.rewind(1)?;
                low
            } else if unchecked > low_mask {
                unchecked - low_threshold
            } else {
                unchecked
            };
            let probability = value as i32 - 1;
            probabilities[symbols] = probability as i16;
            symbols += 1;
            match probability {
                -1 => accumulated += 1,
                value if value > 0 => accumulated += value as u32,
                0 => loop {
                    let zeros = bits.read(2)? as usize;
                    if symbols + zeros > max_symbol + 1 {
                        return Err(DecodeError::InvalidEntropyTable);
                    }
                    symbols += zeros;
                    if zeros != 3 {
                        break;
                    }
                },
                _ => return Err(DecodeError::InvalidEntropyTable),
            }
        }
        if accumulated != expected {
            return Err(DecodeError::InvalidEntropyTable);
        }
        self.build_in_place(&mut probabilities[..symbols], table_log)?;
        Ok(bits.position().div_ceil(8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_counters_cover_the_largest_table_and_ignore_dirty_storage() {
        let mut entries = std::vec![Entry::new(); 512];
        let mut scratch = [i16::MIN; MAX_SYMBOLS];
        let mut table = Table::new(&mut entries);
        table.build(&[512], 9, &mut scratch).unwrap();
        for state in 0..512 {
            let entry = table.entry(state).unwrap();
            assert_eq!(
                (entry.symbol, entry.bits, entry.baseline),
                (0, 0, state as u16)
            );
        }
        // Reusing dirty counters must preserve zero-probability symbols and
        // the special -1 probability without changing the predefined input.
        let probabilities = [16, 0, -1, 15];
        table.build(&probabilities, 5, &mut scratch).unwrap();
        let mut counts = [0; 4];
        for state in 0..32 {
            counts[table.entry(state).unwrap().symbol as usize] += 1;
        }
        assert_eq!(counts, [16, 0, 1, 15]);
        assert_eq!(probabilities, [16, 0, -1, 15]);
    }
}
