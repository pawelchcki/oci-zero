//! Allocation-free extraction of one regular file from a tar stream.

use core::fmt;

const BLOCK_SIZE: usize = 512;
const NAME_RANGE: core::ops::Range<usize> = 0..100;
const SIZE_RANGE: core::ops::Range<usize> = 124..136;
const CHECKSUM_RANGE: core::ops::Range<usize> = 148..156;
const TYPE_OFFSET: usize = 156;
const PREFIX_RANGE: core::ops::Range<usize> = 345..500;

/// A failure encountered while consuming a tar stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractError<E> {
    /// A header checksum did not match its contents.
    InvalidChecksum,
    /// A header contained an invalid or unsupported size field.
    InvalidSize,
    /// A non-zero header followed the first end-of-archive block.
    InvalidEndMarker,
    /// The output callback failed.
    Output(E),
}

impl<E: fmt::Display> fmt::Display for ExtractError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChecksum => formatter.write_str("invalid tar header checksum"),
            Self::InvalidSize => formatter.write_str("invalid tar entry size"),
            Self::InvalidEndMarker => formatter.write_str("invalid tar end marker"),
            Self::Output(error) => write!(formatter, "tar output failed: {error}"),
        }
    }
}

/// A failure detected when the end of the input stream is reported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinishError {
    /// The stream ended inside an entry, a header, or the end marker.
    UnexpectedEof,
    /// The archive ended without a matching regular file.
    NotFound,
}

impl fmt::Display for FinishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedEof => formatter.write_str("unexpected end of tar stream"),
            Self::NotFound => formatter.write_str("tar entry not found"),
        }
    }
}

/// Extracts the first regular file whose path exactly matches `target`.
///
/// The extractor does not allocate. It buffers one 512-byte tar header and
/// passes matching file contents directly to the callback supplied to
/// [`push`](Self::push). Input can be split at arbitrary byte boundaries.
///
/// Paths are compared as raw bytes. POSIX ustar `prefix/name` paths are
/// supported; extension records such as GNU long names and PAX path overrides
/// are intentionally not interpreted.
pub struct EntryExtractor<'target> {
    target: &'target [u8],
    header: [u8; BLOCK_SIZE],
    header_len: usize,
    entry_remaining: u64,
    padding_remaining: usize,
    selected: bool,
    found: bool,
    zero_blocks: u8,
}

impl<'target> EntryExtractor<'target> {
    /// Creates an extractor for an exact archive path.
    pub const fn new(target: &'target [u8]) -> Self {
        Self {
            target,
            header: [0; BLOCK_SIZE],
            header_len: 0,
            entry_remaining: 0,
            padding_remaining: 0,
            selected: false,
            found: false,
            zero_blocks: 0,
        }
    }

    /// Consumes another fragment of the tar byte stream.
    ///
    /// `output` is called only with bytes belonging to the selected regular
    /// file. Once a match has been seen, later entries with the same path are
    /// skipped.
    pub fn push<E>(
        &mut self,
        mut input: &[u8],
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), ExtractError<E>> {
        while !input.is_empty() {
            if self.zero_blocks == 2 {
                return Ok(());
            }

            if self.entry_remaining != 0 {
                let length = input.len().min(u64_to_usize(self.entry_remaining));
                let bytes = &input[..length];
                if self.selected {
                    output(bytes).map_err(ExtractError::Output)?;
                }
                self.entry_remaining -= length as u64;
                input = &input[length..];
                continue;
            }

            if self.padding_remaining != 0 {
                let length = input.len().min(self.padding_remaining);
                self.padding_remaining -= length;
                input = &input[length..];
                continue;
            }

            let length = input.len().min(BLOCK_SIZE - self.header_len);
            self.header[self.header_len..self.header_len + length]
                .copy_from_slice(&input[..length]);
            self.header_len += length;
            input = &input[length..];

            if self.header_len == BLOCK_SIZE {
                self.consume_header()?;
                self.header_len = 0;
            }
        }

        Ok(())
    }

    /// Reports whether the requested entry has already been encountered.
    pub const fn found(&self) -> bool {
        self.found
    }

    /// Validates that the complete archive and requested entry were observed.
    pub fn finish(&self) -> Result<(), FinishError> {
        if self.zero_blocks != 2
            || self.header_len != 0
            || self.entry_remaining != 0
            || self.padding_remaining != 0
        {
            return Err(FinishError::UnexpectedEof);
        }
        if !self.found {
            return Err(FinishError::NotFound);
        }
        Ok(())
    }

    fn consume_header<E>(&mut self) -> Result<(), ExtractError<E>> {
        if self.header.iter().all(|byte| *byte == 0) {
            self.zero_blocks += 1;
            return Ok(());
        }
        if self.zero_blocks != 0 {
            return Err(ExtractError::InvalidEndMarker);
        }

        verify_checksum(&self.header)?;
        let size = parse_number(&self.header[SIZE_RANGE]).ok_or(ExtractError::InvalidSize)?;
        let is_regular = matches!(self.header[TYPE_OFFSET], 0 | b'0');
        self.selected = !self.found && is_regular && path_matches(&self.header, self.target);
        self.found |= self.selected;
        self.entry_remaining = size;
        self.padding_remaining = padding_for(size);
        Ok(())
    }
}

fn verify_checksum<E>(header: &[u8; BLOCK_SIZE]) -> Result<(), ExtractError<E>> {
    let expected = parse_octal(&header[CHECKSUM_RANGE]).ok_or(ExtractError::InvalidChecksum)?;
    let actual = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if CHECKSUM_RANGE.contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum::<u64>();
    if actual == expected {
        Ok(())
    } else {
        Err(ExtractError::InvalidChecksum)
    }
}

fn parse_number(field: &[u8]) -> Option<u64> {
    if field.first().copied()? & 0x80 == 0 {
        return parse_octal(field);
    }

    let mut value = u64::from(field[0] & 0x7f);
    for byte in &field[1..] {
        value = value.checked_mul(256)?.checked_add(u64::from(*byte))?;
    }
    Some(value)
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let mut value = 0u64;
    let mut saw_digit = false;
    let mut saw_terminator = false;

    for byte in field {
        match *byte {
            b'0'..=b'7' if !saw_terminator => {
                saw_digit = true;
                value = value.checked_mul(8)?.checked_add(u64::from(*byte - b'0'))?;
            }
            b' ' if !saw_digit => {}
            0 | b' ' if saw_digit => saw_terminator = true,
            _ => return None,
        }
    }

    saw_digit.then_some(value)
}

fn path_matches(header: &[u8; BLOCK_SIZE], target: &[u8]) -> bool {
    let name = nul_terminated(&header[NAME_RANGE]);
    let prefix = nul_terminated(&header[PREFIX_RANGE]);
    if prefix.is_empty() {
        return target == name;
    }

    target.len() == prefix.len() + 1 + name.len()
        && target.starts_with(prefix)
        && target[prefix.len()] == b'/'
        && &target[prefix.len() + 1..] == name
}

fn nul_terminated(field: &[u8]) -> &[u8] {
    let length = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    &field[..length]
}

fn padding_for(size: u64) -> usize {
    ((BLOCK_SIZE as u64 - size % BLOCK_SIZE as u64) % BLOCK_SIZE as u64) as usize
}

fn u64_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::{EntryExtractor, ExtractError, FinishError, BLOCK_SIZE};
    use std::vec::Vec;

    #[test]
    fn extracts_a_regular_file_from_arbitrary_fragments() {
        let mut archive = Vec::new();
        append_entry(&mut archive, b"first", b"skip", b'0', b"");
        append_entry(&mut archive, b"wanted", b"contents", b'0', b"");
        append_entry(&mut archive, b"wanted", b"later", b'0', b"");
        finish_archive(&mut archive);

        for fragment_size in 1..=BLOCK_SIZE + 1 {
            let mut extractor = EntryExtractor::new(b"wanted");
            let mut output = Vec::new();
            for fragment in archive.chunks(fragment_size) {
                extractor
                    .push(fragment, |bytes| {
                        output.extend_from_slice(bytes);
                        Ok::<_, ()>(())
                    })
                    .unwrap();
            }
            extractor.finish().unwrap();
            assert_eq!(output, b"contents");
        }
    }

    #[test]
    fn matches_a_ustar_prefix_without_allocating_a_path() {
        let mut archive = Vec::new();
        append_entry(
            &mut archive,
            b"settings.yaml",
            b"value",
            b'0',
            b"etc/datadog-agent",
        );
        finish_archive(&mut archive);
        let mut extractor = EntryExtractor::new(b"etc/datadog-agent/settings.yaml");
        let mut output = Vec::new();
        extractor
            .push(&archive, |bytes| {
                output.extend_from_slice(bytes);
                Ok::<_, ()>(())
            })
            .unwrap();
        extractor.finish().unwrap();
        assert_eq!(output, b"value");
    }

    #[test]
    fn recognizes_an_empty_regular_file() {
        let mut archive = Vec::new();
        append_entry(&mut archive, b"empty", b"", b'0', b"");
        finish_archive(&mut archive);
        let mut extractor = EntryExtractor::new(b"empty");
        extractor.push(&archive, |_| Ok::<_, ()>(())).unwrap();
        assert!(extractor.found());
        extractor.finish().unwrap();
    }

    #[test]
    fn does_not_extract_a_directory_with_the_requested_name() {
        let mut archive = Vec::new();
        append_entry(&mut archive, b"wanted", b"", b'5', b"");
        finish_archive(&mut archive);
        let mut extractor = EntryExtractor::new(b"wanted");
        extractor.push(&archive, |_| Ok::<_, ()>(())).unwrap();
        assert_eq!(extractor.finish(), Err(FinishError::NotFound));
    }

    #[test]
    fn rejects_an_invalid_checksum() {
        let mut archive = Vec::new();
        append_entry(&mut archive, b"wanted", b"contents", b'0', b"");
        archive[0] ^= 1;
        let mut extractor = EntryExtractor::new(b"wanted");
        assert_eq!(
            extractor.push(&archive, |_| Ok::<_, ()>(())),
            Err(ExtractError::InvalidChecksum)
        );
    }

    #[test]
    fn reports_a_truncated_stream() {
        let mut archive = Vec::new();
        append_entry(&mut archive, b"wanted", b"contents", b'0', b"");
        let mut extractor = EntryExtractor::new(b"wanted");
        extractor
            .push(&archive[..BLOCK_SIZE + 2], |_| Ok::<_, ()>(()))
            .unwrap();
        assert_eq!(extractor.finish(), Err(FinishError::UnexpectedEof));
    }

    fn append_entry(archive: &mut Vec<u8>, name: &[u8], contents: &[u8], kind: u8, prefix: &[u8]) {
        let mut header = [0u8; BLOCK_SIZE];
        header[..name.len()].copy_from_slice(name);
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        write_octal(&mut header[124..136], contents.len() as u64);
        write_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[345..345 + prefix.len()].copy_from_slice(prefix);
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum();
        write_octal(&mut header[148..156], checksum);

        archive.extend_from_slice(&header);
        archive.extend_from_slice(contents);
        archive.resize(archive.len() + super::padding_for(contents.len() as u64), 0);
    }

    fn finish_archive(archive: &mut Vec<u8>) {
        archive.resize(archive.len() + 2 * BLOCK_SIZE, 0);
    }

    fn write_octal(field: &mut [u8], mut value: u64) {
        field.fill(b'0');
        *field.last_mut().unwrap() = 0;
        let digits = field.len() - 1;
        for byte in field[..digits].iter_mut().rev() {
            *byte = b'0' + (value % 8) as u8;
            value /= 8;
        }
        assert_eq!(value, 0);
    }
}
