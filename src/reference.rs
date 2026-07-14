//! Borrowed OCI references and allocation-free registry paths.

use core::fmt::{self, Write as _};

use crate::digest::{Digest, DigestError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scheme {
    Http,
    Https,
}

impl fmt::Display for Scheme {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Http => "http",
            Self::Https => "https",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selector<'a> {
    Tag(&'a str),
    Digest(Digest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reference<'a> {
    registry: &'a str,
    repository: &'a str,
    selector: Selector<'a>,
}

impl<'a> Reference<'a> {
    pub fn parse(input: &'a str) -> Result<Self, ReferenceError> {
        let value = input
            .strip_prefix("oci://")
            .ok_or(ReferenceError::InvalidScheme)?;
        let (registry, repository_and_selector) = value
            .split_once('/')
            .ok_or(ReferenceError::MissingRepository)?;
        validate_registry(registry)?;

        let (repository, selector) =
            if let Some((repository, digest)) = repository_and_selector.rsplit_once('@') {
                let digest = Digest::parse(digest).map_err(ReferenceError::Digest)?;
                (repository, Selector::Digest(digest))
            } else {
                let (repository, tag) = repository_and_selector
                    .rsplit_once(':')
                    .ok_or(ReferenceError::MissingSelector)?;
                validate_tag(tag)?;
                (repository, Selector::Tag(tag))
            };
        validate_repository(repository)?;
        Ok(Self {
            registry,
            repository,
            selector,
        })
    }

    pub const fn registry(&self) -> &'a str {
        self.registry
    }

    pub const fn repository(&self) -> &'a str {
        self.repository
    }

    pub const fn selector(&self) -> Selector<'a> {
        self.selector
    }

    pub fn manifest_path<'buffer>(
        &self,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, ReferenceError> {
        let mut writer = BufferWriter::new(buffer);
        write!(writer, "/v2/{}/manifests/", self.repository)
            .map_err(|_| ReferenceError::BufferTooSmall)?;
        match self.selector {
            Selector::Tag(tag) => writer
                .write_str(tag)
                .map_err(|_| ReferenceError::BufferTooSmall)?,
            Selector::Digest(digest) => {
                write!(writer, "{digest}").map_err(|_| ReferenceError::BufferTooSmall)?
            }
        }
        writer.finish()
    }

    pub fn manifest_digest_path<'buffer>(
        &self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, ReferenceError> {
        write_path(
            buffer,
            format_args!("/v2/{}/manifests/{digest}", self.repository),
        )
    }

    pub fn blob_path<'buffer>(
        &self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, ReferenceError> {
        write_path(
            buffer,
            format_args!("/v2/{}/blobs/{digest}", self.repository),
        )
    }

    pub fn referrers_path<'buffer>(
        &self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, ReferenceError> {
        write_path(
            buffer,
            format_args!("/v2/{}/referrers/{digest}", self.repository),
        )
    }

    pub fn tags_path<'buffer>(
        &self,
        buffer: &'buffer mut [u8],
    ) -> Result<&'buffer str, ReferenceError> {
        write_path(buffer, format_args!("/v2/{}/tags/list", self.repository))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    InvalidScheme,
    InvalidRegistry,
    MissingRepository,
    InvalidRepository,
    MissingSelector,
    InvalidTag,
    Digest(DigestError),
    BufferTooSmall,
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidScheme => "OCI reference must start with oci://",
            Self::InvalidRegistry => "invalid OCI registry authority",
            Self::MissingRepository => "OCI reference is missing a repository",
            Self::InvalidRepository => "invalid OCI repository name",
            Self::MissingSelector => "OCI reference is missing a tag or digest",
            Self::InvalidTag => "invalid OCI tag",
            Self::Digest(_) => "invalid OCI digest",
            Self::BufferTooSmall => "request path buffer is too small",
        })
    }
}

fn validate_registry(registry: &str) -> Result<(), ReferenceError> {
    if registry.is_empty()
        || !registry.is_ascii()
        || registry
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'@' | b'?' | b'#'))
    {
        return Err(ReferenceError::InvalidRegistry);
    }
    Ok(())
}

fn validate_repository(repository: &str) -> Result<(), ReferenceError> {
    if repository.is_empty() || repository.len() > 255 {
        return Err(ReferenceError::InvalidRepository);
    }
    for component in repository.split('/') {
        let bytes = component.as_bytes();
        if bytes.is_empty()
            || !is_lower_alphanumeric(bytes[0])
            || !is_lower_alphanumeric(*bytes.last().unwrap_or(&0))
        {
            return Err(ReferenceError::InvalidRepository);
        }
        let mut index = 1;
        while index + 1 < bytes.len() {
            if is_lower_alphanumeric(bytes[index]) {
                index += 1;
                continue;
            }
            match bytes[index] {
                b'.' | b'_' => index += 1,
                b'-' => {
                    while index < bytes.len() && bytes[index] == b'-' {
                        index += 1;
                    }
                }
                _ => return Err(ReferenceError::InvalidRepository),
            }
            if index >= bytes.len() || !is_lower_alphanumeric(bytes[index]) {
                return Err(ReferenceError::InvalidRepository);
            }
        }
    }
    Ok(())
}

fn validate_tag(tag: &str) -> Result<(), ReferenceError> {
    let bytes = tag.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !matches!(bytes[0], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
        || !bytes[1..].iter().all(
            |byte| matches!(*byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'-'),
        )
    {
        return Err(ReferenceError::InvalidTag);
    }
    Ok(())
}

const fn is_lower_alphanumeric(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9')
}

fn write_path<'a>(
    buffer: &'a mut [u8],
    arguments: fmt::Arguments<'_>,
) -> Result<&'a str, ReferenceError> {
    let mut writer = BufferWriter::new(buffer);
    writer
        .write_fmt(arguments)
        .map_err(|_| ReferenceError::BufferTooSmall)?;
    writer.finish()
}

struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    length: usize,
}

impl<'a> BufferWriter<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, length: 0 }
    }

    fn finish(self) -> Result<&'a str, ReferenceError> {
        core::str::from_utf8(&self.buffer[..self.length])
            .map_err(|_| ReferenceError::BufferTooSmall)
    }
}

impl fmt::Write for BufferWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.length.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.buffer.get_mut(self.length..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::format;

    use super::{Reference, ReferenceError, Selector};

    #[test]
    fn parses_tags_and_digests() {
        let tagged = Reference::parse("oci://example.com/team/image:v1.2").unwrap();
        assert_eq!(tagged.registry(), "example.com");
        assert_eq!(tagged.repository(), "team/image");
        assert_eq!(tagged.selector(), Selector::Tag("v1.2"));

        let digest = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let input = format!("oci://example.com/image@{digest}");
        let pinned = Reference::parse(&input).unwrap();
        let mut path = [0; 128];
        assert_eq!(
            pinned.manifest_path(&mut path).unwrap(),
            format!("/v2/image/manifests/{digest}")
        );
    }

    #[test]
    fn rejects_invalid_names_and_small_buffers() {
        assert_eq!(
            Reference::parse("oci://example.com/Upper:tag"),
            Err(ReferenceError::InvalidRepository)
        );
        let reference = Reference::parse("oci://example.com/image:tag").unwrap();
        assert_eq!(
            reference.manifest_path(&mut [0; 4]),
            Err(ReferenceError::BufferTooSmall)
        );
    }
}
