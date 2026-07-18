//! Allocation-free extraction of one regular file from a tar stream.

use core::fmt;

const BLOCK_SIZE: usize = 512;
const NAME_RANGE: core::ops::Range<usize> = 0..100;
const SIZE_RANGE: core::ops::Range<usize> = 124..136;
const CHECKSUM_RANGE: core::ops::Range<usize> = 148..156;
const TYPE_OFFSET: usize = 156;
const PREFIX_RANGE: core::ops::Range<usize> = 345..500;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteState {
    Ready,
    File { remaining: u64, size: u64 },
    Finished,
}

/// Allocation-free writer for deterministic ustar archives.
pub struct TarWriter {
    state: WriteState,
}

impl TarWriter {
    pub const fn new() -> Self {
        Self {
            state: WriteState::Ready,
        }
    }

    pub fn begin_file<E>(
        &mut self,
        path: &[u8],
        size: u64,
        mode: u32,
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), TarWriteError<E>> {
        if self.state != WriteState::Ready {
            return Err(TarWriteError::InvalidState);
        }
        if path.is_empty()
            || path.len() > NAME_RANGE.len()
            || path[0] == b'/'
            || path.contains(&0)
            || path
                .split(|byte| *byte == b'/')
                .any(|component| component == b"..")
        {
            return Err(TarWriteError::InvalidPath);
        }

        let mut header = [0u8; BLOCK_SIZE];
        header[..path.len()].copy_from_slice(path);
        write_octal(&mut header[100..108], mode as u64).map_err(cast_write_error)?;
        write_octal(&mut header[108..116], 0).map_err(cast_write_error)?;
        write_octal(&mut header[116..124], 0).map_err(cast_write_error)?;
        write_octal(&mut header[SIZE_RANGE], size).map_err(cast_write_error)?;
        write_octal(&mut header[136..148], 0).map_err(cast_write_error)?;
        header[CHECKSUM_RANGE].fill(b' ');
        header[TYPE_OFFSET] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| *byte as u64).sum();
        write_checksum(&mut header[CHECKSUM_RANGE], checksum).map_err(cast_write_error)?;
        output(&header).map_err(TarWriteError::Output)?;
        self.state = WriteState::File {
            remaining: size,
            size,
        };
        Ok(())
    }

    pub fn write_file_data<E>(
        &mut self,
        bytes: &[u8],
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), TarWriteError<E>> {
        let WriteState::File { remaining, size } = self.state else {
            return Err(TarWriteError::InvalidState);
        };
        if bytes.len() as u64 > remaining {
            return Err(TarWriteError::TooMuchData);
        }
        output(bytes).map_err(TarWriteError::Output)?;
        self.state = WriteState::File {
            remaining: remaining - bytes.len() as u64,
            size,
        };
        Ok(())
    }

    pub fn end_file<E>(
        &mut self,
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), TarWriteError<E>> {
        let WriteState::File { remaining, size } = self.state else {
            return Err(TarWriteError::InvalidState);
        };
        if remaining != 0 {
            return Err(TarWriteError::SizeMismatch { remaining });
        }
        let size_remainder = (size % BLOCK_SIZE as u64) as usize;
        let padding = (BLOCK_SIZE - size_remainder) % BLOCK_SIZE;
        if padding != 0 {
            output(&[0; BLOCK_SIZE][..padding]).map_err(TarWriteError::Output)?;
        }
        self.state = WriteState::Ready;
        Ok(())
    }

    pub fn finish<E>(
        &mut self,
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), TarWriteError<E>> {
        if self.state != WriteState::Ready {
            return Err(TarWriteError::InvalidState);
        }
        output(&[0; BLOCK_SIZE * 2]).map_err(TarWriteError::Output)?;
        self.state = WriteState::Finished;
        Ok(())
    }
}

impl Default for TarWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TarWriteError<E> {
    InvalidState,
    InvalidPath,
    ValueTooLarge,
    TooMuchData,
    SizeMismatch { remaining: u64 },
    Output(E),
}

impl<E: fmt::Display> fmt::Display for TarWriteError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState => formatter.write_str("invalid tar writer state"),
            Self::InvalidPath => formatter.write_str("invalid tar member path"),
            Self::ValueTooLarge => formatter.write_str("tar header value is too large"),
            Self::TooMuchData => formatter.write_str("tar member received too much data"),
            Self::SizeMismatch { remaining } => {
                write!(formatter, "tar member is missing {remaining} bytes")
            }
            Self::Output(error) => write!(formatter, "tar writer output failed: {error}"),
        }
    }
}

fn write_octal(
    field: &mut [u8],
    mut value: u64,
) -> Result<(), TarWriteError<core::convert::Infallible>> {
    field.fill(b'0');
    let digits = field
        .len()
        .checked_sub(1)
        .ok_or(TarWriteError::ValueTooLarge)?;
    field[digits] = 0;
    for index in (0..digits).rev() {
        field[index] = b'0' + (value & 7) as u8;
        value >>= 3;
    }
    if value != 0 {
        return Err(TarWriteError::ValueTooLarge);
    }
    Ok(())
}

fn write_checksum(
    field: &mut [u8],
    mut value: u64,
) -> Result<(), TarWriteError<core::convert::Infallible>> {
    field.fill(b'0');
    field[6] = 0;
    field[7] = b' ';
    for index in (0..6).rev() {
        field[index] = b'0' + (value & 7) as u8;
        value >>= 3;
    }
    if value != 0 {
        return Err(TarWriteError::ValueTooLarge);
    }
    Ok(())
}

fn cast_write_error<E>(error: TarWriteError<core::convert::Infallible>) -> TarWriteError<E> {
    match error {
        TarWriteError::InvalidState => TarWriteError::InvalidState,
        TarWriteError::InvalidPath => TarWriteError::InvalidPath,
        TarWriteError::ValueTooLarge => TarWriteError::ValueTooLarge,
        TarWriteError::TooMuchData => TarWriteError::TooMuchData,
        TarWriteError::SizeMismatch { remaining } => TarWriteError::SizeMismatch { remaining },
        TarWriteError::Output(never) => match never {},
    }
}

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

/// Caller-owned scratch used by [`Archive`].
pub struct ArchiveBuffers<'a> {
    /// Normalized entry path and GNU/PAX path override storage.
    pub path: &'a mut [u8],
    /// Link target and GNU/PAX link override storage.
    pub link: &'a mut [u8],
    /// Raw per-entry PAX records.
    pub pax: &'a mut [u8],
}

/// Type of a tar entry delivered to a [`LayerEventSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Regular,
    HardLink,
    SymbolicLink,
    CharacterDevice,
    BlockDevice,
    Directory,
    Fifo,
    Contiguous,
    Other(u8),
}

/// Metadata for one validated archive entry.
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    pub path: &'a [u8],
    pub kind: EntryKind,
    pub size: u64,
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub mtime: u64,
    pub link_target: Option<&'a [u8]>,
    pub device_major: Option<u64>,
    pub device_minor: Option<u64>,
    pax: &'a [u8],
}

impl<'a> Entry<'a> {
    /// Iterates raw PAX key/value records associated with this entry.
    pub fn pax_records(&self) -> PaxRecords<'a> {
        PaxRecords::new(self.pax)
    }
}

/// One PAX extended-header record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaxRecord<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

/// Iterator over validated PAX records.
pub struct PaxRecords<'a> {
    bytes: &'a [u8],
    position: usize,
    failed: bool,
}

impl<'a> PaxRecords<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            failed: false,
        }
    }
}

impl<'a> Iterator for PaxRecords<'a> {
    type Item = Result<PaxRecord<'a>, PaxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.position == self.bytes.len() {
            return None;
        }
        match parse_pax_record(self.bytes, self.position) {
            Ok((record, next)) => {
                self.position = next;
                Some(Ok(record))
            }
            Err(error) => {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}

/// Receives validated, ordered layer archive events.
pub trait LayerEventSink {
    type Error;

    fn begin_entry(&mut self, entry: Entry<'_>) -> Result<(), Self::Error>;
    fn entry_data(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn end_entry(&mut self) -> Result<(), Self::Error>;
    fn whiteout(&mut self, path: &[u8]) -> Result<(), Self::Error>;
    fn opaque_directory(&mut self, path: &[u8]) -> Result<(), Self::Error>;
}

/// A sink capable of staging a complete layer until integrity checks succeed.
pub trait TransactionalLayerSink: LayerEventSink {
    fn begin_layer(&mut self) -> Result<(), Self::Error>;
    fn commit_layer(&mut self) -> Result<(), Self::Error>;
    fn abort_layer(&mut self);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArchiveEntry {
    None,
    Normal,
    LongPath,
    LongLink,
    Pax,
}

/// Allocation-free streaming tar/PAX layer parser.
pub struct Archive<'a> {
    header: [u8; BLOCK_SIZE],
    header_len: usize,
    entry_remaining: u64,
    padding_remaining: usize,
    current: ArchiveEntry,
    entry_open: bool,
    zero_blocks: u8,
    path: &'a mut [u8],
    path_len: usize,
    link: &'a mut [u8],
    link_len: usize,
    pax: &'a mut [u8],
    pax_len: usize,
    pending_long_path: bool,
    pending_long_link: bool,
    pending_pax: bool,
}

impl<'a> Archive<'a> {
    pub fn new(buffers: ArchiveBuffers<'a>) -> Self {
        Self {
            header: [0; BLOCK_SIZE],
            header_len: 0,
            entry_remaining: 0,
            padding_remaining: 0,
            current: ArchiveEntry::None,
            entry_open: false,
            zero_blocks: 0,
            path: buffers.path,
            path_len: 0,
            link: buffers.link,
            link_len: 0,
            pax: buffers.pax,
            pax_len: 0,
            pending_long_path: false,
            pending_long_link: false,
            pending_pax: false,
        }
    }

    pub fn push<S: LayerEventSink>(
        &mut self,
        mut input: &[u8],
        sink: &mut S,
    ) -> Result<(), ArchiveError<S::Error>> {
        while !input.is_empty() {
            if self.zero_blocks == 2 {
                return Ok(());
            }
            if self.entry_remaining != 0 {
                let length = input.len().min(u64_to_usize(self.entry_remaining));
                let bytes = &input[..length];
                match self.current {
                    ArchiveEntry::Normal => sink.entry_data(bytes).map_err(ArchiveError::Sink)?,
                    ArchiveEntry::LongPath => {
                        append_scratch(self.path, &mut self.path_len, bytes, Scratch::Path)?
                    }
                    ArchiveEntry::LongLink => {
                        append_scratch(self.link, &mut self.link_len, bytes, Scratch::Link)?
                    }
                    ArchiveEntry::Pax => {
                        append_scratch(self.pax, &mut self.pax_len, bytes, Scratch::Pax)?
                    }
                    ArchiveEntry::None => return Err(ArchiveError::InvalidState),
                }
                self.entry_remaining -= length as u64;
                input = &input[length..];
                if self.entry_remaining == 0 {
                    self.finish_content(sink)?;
                }
                continue;
            }
            if self.padding_remaining != 0 {
                let length = input.len().min(self.padding_remaining);
                self.padding_remaining -= length;
                input = &input[length..];
                if self.padding_remaining == 0 {
                    self.current = ArchiveEntry::None;
                }
                continue;
            }

            let length = input.len().min(BLOCK_SIZE - self.header_len);
            self.header[self.header_len..self.header_len + length]
                .copy_from_slice(&input[..length]);
            self.header_len += length;
            input = &input[length..];
            if self.header_len == BLOCK_SIZE {
                self.consume_header(sink)?;
                self.header_len = 0;
            }
        }
        Ok(())
    }

    pub fn finish(&self) -> Result<(), ArchiveFinishError> {
        if self.zero_blocks != 2
            || self.header_len != 0
            || self.entry_remaining != 0
            || self.padding_remaining != 0
            || self.entry_open
            || self.current != ArchiveEntry::None
        {
            return Err(ArchiveFinishError::UnexpectedEof);
        }
        if self.pending_long_path || self.pending_long_link || self.pending_pax {
            return Err(ArchiveFinishError::DanglingExtension);
        }
        Ok(())
    }

    fn consume_header<S: LayerEventSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<(), ArchiveError<S::Error>> {
        if self.header.iter().all(|byte| *byte == 0) {
            if self.current != ArchiveEntry::None {
                return Err(ArchiveError::InvalidState);
            }
            self.zero_blocks += 1;
            return Ok(());
        }
        if self.zero_blocks != 0 {
            return Err(ArchiveError::InvalidEndMarker);
        }
        verify_checksum::<()>(&self.header).map_err(|_| ArchiveError::InvalidChecksum)?;

        let mut size = parse_number(&self.header[SIZE_RANGE]).ok_or(ArchiveError::InvalidSize)?;
        let type_flag = self.header[TYPE_OFFSET];
        match type_flag {
            b'L' | b'K' | b'x' => {
                self.current = match type_flag {
                    b'L' => {
                        self.path_len = 0;
                        ArchiveEntry::LongPath
                    }
                    b'K' => {
                        self.link_len = 0;
                        ArchiveEntry::LongLink
                    }
                    _ => {
                        self.pax_len = 0;
                        ArchiveEntry::Pax
                    }
                };
                self.entry_remaining = size;
                self.padding_remaining = padding_for(size);
                if size == 0 {
                    self.finish_content(sink)?;
                }
                return Ok(());
            }
            b'g' => return Err(ArchiveError::UnsupportedGlobalPax),
            _ => {}
        }

        if self.pending_pax {
            validate_pax(&self.pax[..self.pax_len])?;
            if let Some(value) = pax_value(&self.pax[..self.pax_len], b"size")? {
                size = decimal(value).ok_or(ArchiveError::InvalidSize)?;
            }
        }

        if self.pending_pax {
            if let Some(value) = pax_value(&self.pax[..self.pax_len], b"path")? {
                self.path_len = copy_scratch(self.path, value, Scratch::Path)?;
            } else if !self.pending_long_path {
                self.path_len = header_path(&self.header, self.path)?;
            }
        } else if !self.pending_long_path {
            self.path_len = header_path(&self.header, self.path)?;
        }
        trim_nul(self.path, &mut self.path_len);
        self.path_len = normalize_path(self.path, self.path_len)?;

        if self.pending_pax {
            if let Some(value) = pax_value(&self.pax[..self.pax_len], b"linkpath")? {
                self.link_len = copy_scratch(self.link, value, Scratch::Link)?;
            } else if !self.pending_long_link {
                self.link_len = copy_scratch(
                    self.link,
                    nul_terminated(&self.header[157..257]),
                    Scratch::Link,
                )?;
            }
        } else if !self.pending_long_link {
            self.link_len = copy_scratch(
                self.link,
                nul_terminated(&self.header[157..257]),
                Scratch::Link,
            )?;
        }
        trim_nul(self.link, &mut self.link_len);

        let kind = entry_kind(type_flag);
        match kind {
            EntryKind::HardLink if self.link_len != 0 => {
                self.link_len = normalize_path(self.link, self.link_len)?;
            }
            EntryKind::SymbolicLink if self.link_len != 0 => {
                validate_symbolic_link(&self.path[..self.path_len], &self.link[..self.link_len])?;
            }
            _ => {}
        }

        self.pending_long_path = false;
        self.pending_long_link = false;
        self.current = ArchiveEntry::Normal;
        self.entry_remaining = size;
        self.padding_remaining = padding_for(size);

        if let Some(whiteout) = whiteout(self.path, &mut self.path_len) {
            if kind != EntryKind::Regular || size != 0 {
                return Err(ArchiveError::InvalidWhiteout);
            }
            match whiteout {
                Whiteout::Remove => sink
                    .whiteout(&self.path[..self.path_len])
                    .map_err(ArchiveError::Sink)?,
                Whiteout::Opaque => sink
                    .opaque_directory(&self.path[..self.path_len])
                    .map_err(ArchiveError::Sink)?,
            }
            self.pending_pax = false;
            self.pax_len = 0;
            self.current = ArchiveEntry::None;
            return Ok(());
        }

        let mode = parse_number(&self.header[100..108]).ok_or(ArchiveError::InvalidMetadata)?;
        let uid = parse_number(&self.header[108..116]).ok_or(ArchiveError::InvalidMetadata)?;
        let gid = parse_number(&self.header[116..124]).ok_or(ArchiveError::InvalidMetadata)?;
        let mtime = parse_number(&self.header[136..148]).ok_or(ArchiveError::InvalidMetadata)?;
        let has_device = matches!(kind, EntryKind::CharacterDevice | EntryKind::BlockDevice);
        let device_major = has_device
            .then(|| parse_number(&self.header[329..337]))
            .flatten();
        let device_minor = has_device
            .then(|| parse_number(&self.header[337..345]))
            .flatten();
        if has_device && (device_major.is_none() || device_minor.is_none()) {
            return Err(ArchiveError::InvalidMetadata);
        }
        let link_target = matches!(kind, EntryKind::HardLink | EntryKind::SymbolicLink)
            .then_some(&self.link[..self.link_len]);
        sink.begin_entry(Entry {
            path: &self.path[..self.path_len],
            kind,
            size,
            mode,
            uid,
            gid,
            mtime,
            link_target,
            device_major,
            device_minor,
            pax: if self.pending_pax {
                &self.pax[..self.pax_len]
            } else {
                &[]
            },
        })
        .map_err(ArchiveError::Sink)?;
        self.entry_open = true;
        self.pending_pax = false;
        self.pax_len = 0;
        if size == 0 {
            self.finish_content(sink)?;
        }
        Ok(())
    }

    fn finish_content<S: LayerEventSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<(), ArchiveError<S::Error>> {
        match self.current {
            ArchiveEntry::Normal => {
                if self.entry_open {
                    sink.end_entry().map_err(ArchiveError::Sink)?;
                    self.entry_open = false;
                }
            }
            ArchiveEntry::LongPath => {
                trim_nul(self.path, &mut self.path_len);
                self.pending_long_path = true;
            }
            ArchiveEntry::LongLink => {
                trim_nul(self.link, &mut self.link_len);
                self.pending_long_link = true;
            }
            ArchiveEntry::Pax => {
                validate_pax(&self.pax[..self.pax_len])?;
                self.pending_pax = true;
            }
            ArchiveEntry::None => return Err(ArchiveError::InvalidState),
        }
        if self.padding_remaining == 0 {
            self.current = ArchiveEntry::None;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scratch {
    Path,
    Link,
    Pax,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ArchiveError<E> {
    InvalidChecksum,
    InvalidSize,
    InvalidMetadata,
    InvalidEndMarker,
    InvalidPath,
    InvalidLink,
    InvalidWhiteout,
    InvalidPax(PaxError),
    UnsupportedGlobalPax,
    BufferTooSmall(Scratch),
    InvalidState,
    Sink(E),
}

impl<E: fmt::Display> fmt::Display for ArchiveError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChecksum => formatter.write_str("invalid tar header checksum"),
            Self::InvalidSize => formatter.write_str("invalid tar entry size"),
            Self::InvalidMetadata => formatter.write_str("invalid tar entry metadata"),
            Self::InvalidEndMarker => formatter.write_str("invalid tar end marker"),
            Self::InvalidPath => formatter.write_str("unsafe or invalid tar path"),
            Self::InvalidLink => formatter.write_str("unsafe tar link target"),
            Self::InvalidWhiteout => formatter.write_str("invalid OCI whiteout entry"),
            Self::InvalidPax(error) => write!(formatter, "invalid PAX header: {error}"),
            Self::UnsupportedGlobalPax => {
                formatter.write_str("global PAX headers are not supported")
            }
            Self::BufferTooSmall(buffer) => write!(formatter, "{buffer:?} buffer is too small"),
            Self::InvalidState => formatter.write_str("invalid tar parser state"),
            Self::Sink(error) => write!(formatter, "layer sink failed: {error}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFinishError {
    UnexpectedEof,
    DanglingExtension,
}

impl fmt::Display for ArchiveFinishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnexpectedEof => "unexpected end of tar archive",
            Self::DanglingExtension => "tar archive ended after an extension header",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaxError {
    InvalidLength,
    InvalidRecord,
}

impl fmt::Display for PaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidLength => "invalid record length",
            Self::InvalidRecord => "invalid key/value record",
        })
    }
}

impl<E> From<PaxError> for ArchiveError<E> {
    fn from(error: PaxError) -> Self {
        Self::InvalidPax(error)
    }
}

#[derive(Clone, Copy)]
enum Whiteout {
    Remove,
    Opaque,
}

fn entry_kind(type_flag: u8) -> EntryKind {
    match type_flag {
        0 | b'0' => EntryKind::Regular,
        b'1' => EntryKind::HardLink,
        b'2' => EntryKind::SymbolicLink,
        b'3' => EntryKind::CharacterDevice,
        b'4' => EntryKind::BlockDevice,
        b'5' => EntryKind::Directory,
        b'6' => EntryKind::Fifo,
        b'7' => EntryKind::Contiguous,
        other => EntryKind::Other(other),
    }
}

fn header_path<E>(
    header: &[u8; BLOCK_SIZE],
    destination: &mut [u8],
) -> Result<usize, ArchiveError<E>> {
    let name = nul_terminated(&header[NAME_RANGE]);
    let prefix = nul_terminated(&header[PREFIX_RANGE]);
    let needed = name.len() + usize::from(!prefix.is_empty()) + prefix.len();
    if needed > destination.len() {
        return Err(ArchiveError::BufferTooSmall(Scratch::Path));
    }
    let mut length = 0;
    if !prefix.is_empty() {
        destination[..prefix.len()].copy_from_slice(prefix);
        length += prefix.len();
        destination[length] = b'/';
        length += 1;
    }
    destination[length..length + name.len()].copy_from_slice(name);
    Ok(length + name.len())
}

fn append_scratch<E>(
    destination: &mut [u8],
    length: &mut usize,
    bytes: &[u8],
    kind: Scratch,
) -> Result<(), ArchiveError<E>> {
    let end = length
        .checked_add(bytes.len())
        .ok_or(ArchiveError::BufferTooSmall(kind))?;
    let output = destination
        .get_mut(*length..end)
        .ok_or(ArchiveError::BufferTooSmall(kind))?;
    output.copy_from_slice(bytes);
    *length = end;
    Ok(())
}

fn copy_scratch<E>(
    destination: &mut [u8],
    bytes: &[u8],
    kind: Scratch,
) -> Result<usize, ArchiveError<E>> {
    if bytes.len() > destination.len() {
        return Err(ArchiveError::BufferTooSmall(kind));
    }
    destination[..bytes.len()].copy_from_slice(bytes);
    Ok(bytes.len())
}

fn trim_nul(bytes: &[u8], length: &mut usize) {
    while *length != 0 && bytes[*length - 1] == 0 {
        *length -= 1;
    }
}

fn normalize_path<E>(bytes: &mut [u8], length: usize) -> Result<usize, ArchiveError<E>> {
    if length == 0 || bytes[0] == b'/' || bytes[..length].contains(&0) {
        return Err(ArchiveError::InvalidPath);
    }
    let mut read = 0;
    let mut write = 0;
    while read < length {
        while read < length && bytes[read] == b'/' {
            read += 1;
        }
        let start = read;
        while read < length && bytes[read] != b'/' {
            read += 1;
        }
        let component_len = read - start;
        if component_len == 0 || &bytes[start..read] == b"." {
            continue;
        }
        if &bytes[start..read] == b".." {
            return Err(ArchiveError::InvalidPath);
        }
        if write != 0 {
            bytes[write] = b'/';
            write += 1;
        }
        bytes.copy_within(start..read, write);
        write += component_len;
    }
    if write == 0 {
        bytes[0] = b'.';
        write = 1;
    }
    Ok(write)
}

fn validate_symbolic_link<E>(path: &[u8], target: &[u8]) -> Result<(), ArchiveError<E>> {
    if target.is_empty() || target.contains(&0) {
        return Err(ArchiveError::InvalidLink);
    }
    let mut depth = if target[0] == b'/' {
        0
    } else {
        path.split(|byte| *byte == b'/').count().saturating_sub(1)
    };
    for component in target.split(|byte| *byte == b'/') {
        match component {
            b"" | b"." => {}
            b".." => {
                depth = depth.checked_sub(1).ok_or(ArchiveError::InvalidLink)?;
            }
            _ => depth += 1,
        }
    }
    Ok(())
}

fn whiteout(path: &mut [u8], length: &mut usize) -> Option<Whiteout> {
    let basename = path[..*length]
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(0, |position| position + 1);
    let name = &path[basename..*length];
    if name == b".wh..wh..opq" {
        if basename == 0 {
            path[0] = b'.';
            *length = 1;
        } else {
            *length = basename - 1;
        }
        return Some(Whiteout::Opaque);
    }
    let target = name.strip_prefix(b".wh.")?;
    if target.is_empty() {
        return None;
    }
    path.copy_within(basename + 4..*length, basename);
    *length -= 4;
    Some(Whiteout::Remove)
}

fn validate_pax<E>(bytes: &[u8]) -> Result<(), ArchiveError<E>> {
    for record in PaxRecords::new(bytes) {
        record?;
    }
    Ok(())
}

fn pax_value<'a, E>(bytes: &'a [u8], key: &[u8]) -> Result<Option<&'a [u8]>, ArchiveError<E>> {
    let mut found = None;
    for record in PaxRecords::new(bytes) {
        let record = record?;
        if record.key == key {
            if found.is_some() {
                return Err(ArchiveError::InvalidPax(PaxError::InvalidRecord));
            }
            found = Some(record.value);
        }
    }
    Ok(found)
}

fn parse_pax_record(bytes: &[u8], position: usize) -> Result<(PaxRecord<'_>, usize), PaxError> {
    let space = bytes[position..]
        .iter()
        .position(|byte| *byte == b' ')
        .map(|offset| position + offset)
        .ok_or(PaxError::InvalidLength)?;
    let length = decimal(&bytes[position..space]).ok_or(PaxError::InvalidLength)?;
    let length = usize::try_from(length).map_err(|_| PaxError::InvalidLength)?;
    let end = position
        .checked_add(length)
        .ok_or(PaxError::InvalidLength)?;
    let record = bytes.get(space + 1..end).ok_or(PaxError::InvalidLength)?;
    let record = record.strip_suffix(b"\n").ok_or(PaxError::InvalidRecord)?;
    let equals = record
        .iter()
        .position(|byte| *byte == b'=')
        .ok_or(PaxError::InvalidRecord)?;
    if equals == 0 {
        return Err(PaxError::InvalidRecord);
    }
    Ok((
        PaxRecord {
            key: &record[..equals],
            value: &record[equals + 1..],
        },
        end,
    ))
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |value, byte| {
        value.checked_mul(10)?.checked_add(u64::from(
            byte.checked_sub(b'0').filter(|digit| *digit <= 9)?,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, ArchiveBuffers, Entry, EntryExtractor, EntryKind, ExtractError, FinishError,
        LayerEventSink, TarWriteError, TarWriter, BLOCK_SIZE,
    };
    use std::{format, string::ToString, vec::Vec};

    type OwnedPaxRecord = (Vec<u8>, Vec<u8>);
    type OwnedEntry = (Vec<u8>, EntryKind, Vec<OwnedPaxRecord>);

    #[derive(Default)]
    struct Events {
        entries: Vec<OwnedEntry>,
        contents: Vec<u8>,
        whiteouts: Vec<Vec<u8>>,
        opaque: Vec<Vec<u8>>,
    }

    #[test]
    fn writes_deterministic_ustar_files() {
        let mut bytes = Vec::new();
        let mut writer = TarWriter::new();
        writer
            .begin_file(b"blobs/sha256/abc", 5, 0o644, |chunk| {
                bytes.extend_from_slice(chunk);
                Ok::<_, ()>(())
            })
            .unwrap();
        writer
            .write_file_data(b"hello", |chunk| {
                bytes.extend_from_slice(chunk);
                Ok::<_, ()>(())
            })
            .unwrap();
        writer
            .end_file(|chunk| {
                bytes.extend_from_slice(chunk);
                Ok::<_, ()>(())
            })
            .unwrap();
        writer
            .finish(|chunk| {
                bytes.extend_from_slice(chunk);
                Ok::<_, ()>(())
            })
            .unwrap();

        assert_eq!(&bytes[..18], b"blobs/sha256/abc\0\0");
        assert_eq!(&bytes[BLOCK_SIZE..BLOCK_SIZE + 5], b"hello");
        assert_eq!(bytes.len(), BLOCK_SIZE * 4);

        let mut invalid = TarWriter::new();
        assert_eq!(
            invalid.begin_file(b"../escape", 0, 0o644, |_| Ok::<_, ()>(())),
            Err(TarWriteError::InvalidPath)
        );
    }

    impl LayerEventSink for Events {
        type Error = ();

        fn begin_entry(&mut self, entry: Entry<'_>) -> Result<(), Self::Error> {
            let pax = entry
                .pax_records()
                .map(|record| {
                    let record = record.unwrap();
                    (record.key.to_vec(), record.value.to_vec())
                })
                .collect();
            self.entries.push((entry.path.to_vec(), entry.kind, pax));
            Ok(())
        }

        fn entry_data(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
            self.contents.extend_from_slice(bytes);
            Ok(())
        }

        fn end_entry(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn whiteout(&mut self, path: &[u8]) -> Result<(), Self::Error> {
            self.whiteouts.push(path.to_vec());
            Ok(())
        }

        fn opaque_directory(&mut self, path: &[u8]) -> Result<(), Self::Error> {
            self.opaque.push(path.to_vec());
            Ok(())
        }
    }

    #[test]
    fn streams_layer_events_and_whiteouts_at_every_fragment_size() {
        let mut bytes = Vec::new();
        append_entry(&mut bytes, b"./etc//config", b"value", b'0', b"");
        append_entry(&mut bytes, b"etc/.wh.deleted", b"", b'0', b"");
        append_entry(&mut bytes, b"var/lib/.wh..wh..opq", b"", b'0', b"");
        finish_archive(&mut bytes);

        for fragment in 1..=BLOCK_SIZE + 1 {
            let mut path = [0; 256];
            let mut link = [0; 256];
            let mut pax = [0; 512];
            let mut archive = Archive::new(ArchiveBuffers {
                path: &mut path,
                link: &mut link,
                pax: &mut pax,
            });
            let mut events = Events::default();
            for chunk in bytes.chunks(fragment) {
                archive.push(chunk, &mut events).unwrap();
            }
            archive.finish().unwrap();
            assert_eq!(events.entries[0].0, b"etc/config");
            assert_eq!(events.contents, b"value");
            assert_eq!(events.whiteouts, [b"etc/deleted".to_vec()]);
            assert_eq!(events.opaque, [b"var/lib".to_vec()]);
        }
    }

    #[test]
    fn applies_pax_and_gnu_long_path_overrides_without_allocating() {
        let mut bytes = Vec::new();
        let record = pax_record("path", "pax/overridden/file");
        append_entry(&mut bytes, b"PaxHeaders.0", &record, b'x', b"");
        append_entry(&mut bytes, b"ignored", b"pax", b'0', b"");

        let long = [b'a'; 140];
        let mut long_nul = long.to_vec();
        long_nul.push(0);
        append_entry(&mut bytes, b"LongLink", &long_nul, b'L', b"");
        append_entry(&mut bytes, b"ignored", b"gnu", b'0', b"");
        finish_archive(&mut bytes);

        let mut path = [0; 256];
        let mut link = [0; 256];
        let mut pax = [0; 512];
        let mut archive = Archive::new(ArchiveBuffers {
            path: &mut path,
            link: &mut link,
            pax: &mut pax,
        });
        let mut events = Events::default();
        archive.push(&bytes, &mut events).unwrap();
        archive.finish().unwrap();
        assert_eq!(events.entries[0].0, b"pax/overridden/file");
        assert_eq!(events.entries[0].2[0].0, b"path");
        assert_eq!(events.entries[1].0, long);
        assert_eq!(events.contents, b"paxgnu");
    }

    #[test]
    fn rejects_paths_that_escape_the_root() {
        let mut bytes = Vec::new();
        append_entry(&mut bytes, b"../escape", b"", b'0', b"");
        let mut path = [0; 64];
        let mut link = [0; 64];
        let mut pax = [0; 64];
        let mut archive = Archive::new(ArchiveBuffers {
            path: &mut path,
            link: &mut link,
            pax: &mut pax,
        });
        assert!(archive.push(&bytes, &mut Events::default()).is_err());
    }

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

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let payload = key.len() + 1 + value.len() + 1;
        let mut digits = 1;
        loop {
            let length = digits + 1 + payload;
            let actual_digits = length.to_string().len();
            if actual_digits == digits {
                return format!("{length} {key}={value}\n").into_bytes();
            }
            digits = actual_digits;
        }
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
