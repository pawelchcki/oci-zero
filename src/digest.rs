//! OCI content digests and streaming verification.

use core::fmt;

use sha2::{Digest as _, Sha256};

/// Number of bytes in a SHA-256 digest.
pub const SHA256_SIZE: usize = 32;

/// A supported OCI content digest.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Digest([u8; SHA256_SIZE]);

impl Digest {
    /// Parses a canonical `sha256:<64 lowercase hexadecimal digits>` digest.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        let encoded = value
            .strip_prefix("sha256:")
            .ok_or(DigestError::UnsupportedAlgorithm)?;
        if encoded.len() != SHA256_SIZE * 2 {
            return Err(DigestError::InvalidLength);
        }
        let mut bytes = [0; SHA256_SIZE];
        for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
            bytes[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
        }
        Ok(Self(bytes))
    }

    pub const fn from_bytes(bytes: [u8; SHA256_SIZE]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; SHA256_SIZE] {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Incremental verifier for a descriptor payload.
pub struct Verifier {
    expected_digest: Digest,
    expected_size: Option<u64>,
    actual_size: u64,
    hash: Sha256,
}

impl Verifier {
    pub fn new(expected_digest: Digest, expected_size: u64) -> Self {
        Self {
            expected_digest,
            expected_size: Some(expected_size),
            actual_size: 0,
            hash: Sha256::new(),
        }
    }

    /// Creates a verifier for content whose size is not described separately.
    pub fn digest_only(expected_digest: Digest) -> Self {
        Self {
            expected_digest,
            expected_size: None,
            actual_size: 0,
            hash: Sha256::new(),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<(), VerifyError> {
        self.actual_size = self
            .actual_size
            .checked_add(bytes.len() as u64)
            .ok_or(VerifyError::SizeOverflow)?;
        self.hash.update(bytes);
        Ok(())
    }

    pub const fn actual_size(&self) -> u64 {
        self.actual_size
    }

    pub fn finish(&self) -> Result<(), VerifyError> {
        if let Some(expected_size) = self.expected_size.filter(|size| *size != self.actual_size) {
            return Err(VerifyError::SizeMismatch {
                expected: expected_size,
                actual: self.actual_size,
            });
        }
        let actual = Digest::from_bytes(self.hash.clone().finalize().into());
        if actual != self.expected_digest {
            return Err(VerifyError::DigestMismatch {
                expected: self.expected_digest,
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DigestError {
    UnsupportedAlgorithm,
    InvalidLength,
    InvalidEncoding,
}

impl fmt::Display for DigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedAlgorithm => "unsupported digest algorithm",
            Self::InvalidLength => "invalid SHA-256 digest length",
            Self::InvalidEncoding => "invalid SHA-256 digest encoding",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifyError {
    SizeOverflow,
    SizeMismatch { expected: u64, actual: u64 },
    DigestMismatch { expected: Digest, actual: Digest },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SizeOverflow => formatter.write_str("content size overflow"),
            Self::SizeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "size mismatch: expected {expected}, got {actual}"
                )
            }
            Self::DigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "digest mismatch: expected {expected}, got {actual}"
                )
            }
        }
    }
}

fn hex(byte: u8) -> Result<u8, DigestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DigestError::InvalidEncoding),
    }
}

#[cfg(test)]
mod tests {
    use std::string::ToString;

    use super::{Digest, Verifier, VerifyError};

    #[test]
    fn parses_and_formats_sha256() {
        let text = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(Digest::parse(text).unwrap().to_string(), text);
    }

    #[test]
    fn verifies_fragmented_content() {
        let digest = Digest::parse(
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        let mut verifier = Verifier::new(digest, 3);
        verifier.update(b"a").unwrap();
        verifier.update(b"bc").unwrap();
        assert_eq!(verifier.finish(), Ok(()));

        let mut verifier = Verifier::new(digest, 4);
        verifier.update(b"abc").unwrap();
        assert!(matches!(
            verifier.finish(),
            Err(VerifyError::SizeMismatch { .. })
        ));
    }
}
