//! Checked writes into caller-owned storage, shared by protocol serializers.

use core::{fmt, str};

pub(crate) fn append(buffer: &mut [u8], length: &mut usize, bytes: &[u8]) -> fmt::Result {
    let end = length.checked_add(bytes.len()).ok_or(fmt::Error)?;
    buffer
        .get_mut(*length..end)
        .ok_or(fmt::Error)?
        .copy_from_slice(bytes);
    *length = end;
    Ok(())
}

pub(crate) struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

impl<'a> BufferWriter<'a> {
    pub(crate) fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, length: 0 }
    }

    pub(crate) fn len(&self) -> usize {
        self.length
    }

    pub(crate) fn finish(self) -> Result<&'a str, fmt::Error> {
        str::from_utf8(&self.buffer[..self.length]).map_err(|_| fmt::Error)
    }
}

impl fmt::Write for BufferWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        append(self.buffer, &mut self.length, value.as_bytes())
    }
}
