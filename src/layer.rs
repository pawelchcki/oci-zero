//! Verified, allocation-free OCI layer decoding.

use core::fmt;

use crate::{
    digest::{Digest, Verifier, VerifyError},
    tar::{Archive, ArchiveError, ArchiveFinishError, TransactionalLayerSink},
};

pub const OCI_LAYER_TAR: &str = "application/vnd.oci.image.layer.v1.tar";
pub const OCI_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
pub const OCI_LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
pub const OCI_NONDISTRIBUTABLE_TAR: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar";
pub const OCI_NONDISTRIBUTABLE_GZIP: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar+gzip";
pub const OCI_NONDISTRIBUTABLE_ZSTD: &str =
    "application/vnd.oci.image.layer.nondistributable.v1.tar+zstd";
pub const DOCKER_LAYER_GZIP: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
pub const DOCKER_FOREIGN_LAYER_GZIP: &str =
    "application/vnd.docker.image.rootfs.foreign.diff.tar.gzip";
pub const DOCKER_LAYER_TAR: &str = "application/vnd.docker.image.rootfs.diff.tar";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoding {
    Tar,
    Gzip,
    Zstd,
}

pub fn encoding(media_type: &str) -> Result<Encoding, LayerFormatError> {
    match media_type {
        OCI_LAYER_TAR | OCI_NONDISTRIBUTABLE_TAR | DOCKER_LAYER_TAR => Ok(Encoding::Tar),
        OCI_LAYER_GZIP
        | OCI_NONDISTRIBUTABLE_GZIP
        | DOCKER_LAYER_GZIP
        | DOCKER_FOREIGN_LAYER_GZIP => Ok(Encoding::Gzip),
        OCI_LAYER_ZSTD | OCI_NONDISTRIBUTABLE_ZSTD => Ok(Encoding::Zstd),
        _ => Err(LayerFormatError::UnsupportedMediaType),
    }
}

// Keeping codec state inline is intentional: indirection would require an
// allocator or a second caller-owned object with a more fragile lifetime API.
#[allow(clippy::large_enum_variant)]
pub enum Decoder<'a> {
    Tar,
    #[cfg(feature = "gzip")]
    Gzip(gzip_zero::Decoder<'a>),
    #[cfg(feature = "zstd")]
    Zstd(zstd_zero::Decoder<'a>),
    #[doc(hidden)]
    _Lifetime(core::marker::PhantomData<&'a mut [u8]>),
}

impl<'a> Decoder<'a> {
    pub const fn tar() -> Self {
        Self::Tar
    }

    #[cfg(feature = "gzip")]
    pub fn gzip(buffers: gzip_zero::DecoderBuffers<'a>) -> Result<Self, LayerFormatError> {
        Ok(Self::Gzip(
            gzip_zero::Decoder::new(buffers).map_err(LayerFormatError::Gzip)?,
        ))
    }

    #[cfg(feature = "zstd")]
    pub fn zstd(buffers: zstd_zero::DecoderBuffers<'a>) -> Self {
        Self::Zstd(zstd_zero::Decoder::new(buffers))
    }

    pub fn decode<'decoder>(
        &'decoder mut self,
        input: &'decoder [u8],
    ) -> Result<DecodeStep<'decoder>, LayerFormatError> {
        match self {
            Self::Tar => Ok(if input.is_empty() {
                DecodeStep::NeedInput { consumed: 0 }
            } else {
                DecodeStep::Output {
                    consumed: input.len(),
                    bytes: input,
                }
            }),
            #[cfg(feature = "gzip")]
            Self::Gzip(decoder) => Ok(
                match decoder.decode(input).map_err(LayerFormatError::Gzip)? {
                    gzip_zero::DecodeStep::NeedInput { consumed } => {
                        DecodeStep::NeedInput { consumed }
                    }
                    gzip_zero::DecodeStep::MemberStarted { consumed, .. }
                    | gzip_zero::DecodeStep::MemberFinished { consumed } => {
                        DecodeStep::Progress { consumed }
                    }
                    gzip_zero::DecodeStep::Output { consumed, bytes } => {
                        DecodeStep::Output { consumed, bytes }
                    }
                },
            ),
            #[cfg(feature = "zstd")]
            Self::Zstd(decoder) => Ok(
                match decoder.decode(input).map_err(LayerFormatError::Zstd)? {
                    zstd_zero::DecodeStep::NeedInput { consumed } => {
                        DecodeStep::NeedInput { consumed }
                    }
                    zstd_zero::DecodeStep::FrameStarted { consumed, .. }
                    | zstd_zero::DecodeStep::FrameFinished { consumed, .. } => {
                        DecodeStep::Progress { consumed }
                    }
                    zstd_zero::DecodeStep::Output { consumed, bytes } => {
                        DecodeStep::Output { consumed, bytes }
                    }
                },
            ),
            Self::_Lifetime(_) => unreachable!(),
        }
    }

    pub fn finish(&self) -> Result<(), LayerFormatError> {
        match self {
            Self::Tar => Ok(()),
            #[cfg(feature = "gzip")]
            Self::Gzip(decoder) => decoder.finish().map_err(LayerFormatError::Gzip),
            #[cfg(feature = "zstd")]
            Self::Zstd(decoder) => decoder.finish().map_err(LayerFormatError::Zstd),
            Self::_Lifetime(_) => unreachable!(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecodeStep<'a> {
    NeedInput { consumed: usize },
    Progress { consumed: usize },
    Output { consumed: usize, bytes: &'a [u8] },
}

impl DecodeStep<'_> {
    pub const fn consumed(&self) -> usize {
        match self {
            Self::NeedInput { consumed }
            | Self::Progress { consumed }
            | Self::Output { consumed, .. } => *consumed,
        }
    }
}

pub struct VerifiedDecoder<'a> {
    decoder: Decoder<'a>,
    compressed: Verifier,
    uncompressed: Verifier,
}

/// Transactionally decodes, verifies, parses, and applies one OCI layer.
pub struct LayerApplier<'decoder, 'archive> {
    decoder: VerifiedDecoder<'decoder>,
    archive: Archive<'archive>,
    state: ApplyState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyState {
    Ready,
    Applying,
    Failed,
    Finished,
}

impl<'decoder, 'archive> LayerApplier<'decoder, 'archive> {
    pub fn new(decoder: VerifiedDecoder<'decoder>, archive: Archive<'archive>) -> Self {
        Self {
            decoder,
            archive,
            state: ApplyState::Ready,
        }
    }

    pub fn push<S: TransactionalLayerSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
    ) -> Result<(), ApplyError<S::Error>> {
        match self.state {
            ApplyState::Ready => {
                sink.begin_layer().map_err(ApplyError::Sink)?;
                self.state = ApplyState::Applying;
            }
            ApplyState::Applying => {}
            ApplyState::Failed | ApplyState::Finished => return Err(ApplyError::InvalidState),
        }
        let (decoder, archive) = (&mut self.decoder, &mut self.archive);
        let result = decoder.push(input, |bytes| archive.push(bytes, sink));
        if let Err(error) = result {
            sink.abort_layer();
            self.state = ApplyState::Failed;
            return Err(map_apply_error(error));
        }
        Ok(())
    }

    pub fn finish<S: TransactionalLayerSink>(
        &mut self,
        sink: &mut S,
    ) -> Result<(), ApplyError<S::Error>> {
        if self.state == ApplyState::Ready {
            sink.begin_layer().map_err(ApplyError::Sink)?;
            self.state = ApplyState::Applying;
        }
        if self.state != ApplyState::Applying {
            return Err(ApplyError::InvalidState);
        }
        let (decoder, archive) = (&mut self.decoder, &mut self.archive);
        if let Err(error) = decoder.finish(|bytes| archive.push(bytes, sink)) {
            sink.abort_layer();
            self.state = ApplyState::Failed;
            return Err(map_apply_error(error));
        }
        if let Err(error) = archive.finish() {
            sink.abort_layer();
            self.state = ApplyState::Failed;
            return Err(ApplyError::ArchiveFinish(error));
        }
        if let Err(error) = sink.commit_layer() {
            sink.abort_layer();
            self.state = ApplyState::Failed;
            return Err(ApplyError::Sink(error));
        }
        self.state = ApplyState::Finished;
        Ok(())
    }
}

#[derive(Debug)]
pub enum ApplyError<E> {
    Format(LayerFormatError),
    Integrity(VerifyError),
    Archive(ArchiveError<E>),
    ArchiveFinish(ArchiveFinishError),
    Sink(E),
    InvalidState,
}

impl<E: fmt::Display> fmt::Display for ApplyError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => write!(formatter, "layer format error: {error}"),
            Self::Integrity(error) => write!(formatter, "layer integrity error: {error}"),
            Self::Archive(error) => write!(formatter, "layer archive error: {error}"),
            Self::ArchiveFinish(error) => write!(formatter, "layer archive error: {error}"),
            Self::Sink(error) => write!(formatter, "layer sink failed: {error}"),
            Self::InvalidState => formatter.write_str("invalid layer application state"),
        }
    }
}

fn map_apply_error<E>(error: LayerError<ArchiveError<E>>) -> ApplyError<E> {
    match error {
        LayerError::Format(error) => ApplyError::Format(error),
        LayerError::Integrity(error) => ApplyError::Integrity(error),
        LayerError::Output(error) => ApplyError::Archive(error),
    }
}

impl<'a> VerifiedDecoder<'a> {
    pub fn new(
        decoder: Decoder<'a>,
        compressed_digest: Digest,
        compressed_size: u64,
        diff_id: Digest,
    ) -> Self {
        Self {
            decoder,
            compressed: Verifier::new(compressed_digest, compressed_size),
            uncompressed: Verifier::digest_only(diff_id),
        }
    }

    pub fn push<E>(
        &mut self,
        input: &[u8],
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), LayerError<E>> {
        self.compressed
            .update(input)
            .map_err(LayerError::Integrity)?;
        let mut remaining = input;
        let mut turns_without_bytes = 0usize;
        while !remaining.is_empty() {
            let step = self.decoder.decode(remaining).map_err(LayerError::Format)?;
            let consumed = step.consumed();
            if let DecodeStep::Output { bytes, .. } = step {
                self.uncompressed
                    .update(bytes)
                    .map_err(LayerError::Integrity)?;
                output(bytes).map_err(LayerError::Output)?;
            }
            if consumed == 0 {
                turns_without_bytes += 1;
                if turns_without_bytes > 16 {
                    return Err(LayerError::Format(LayerFormatError::DecoderStalled));
                }
            } else {
                turns_without_bytes = 0;
                remaining = &remaining[consumed..];
            }
        }
        Ok(())
    }

    pub fn finish<E>(
        &mut self,
        mut output: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), LayerError<E>> {
        loop {
            match self.decoder.decode(&[]).map_err(LayerError::Format)? {
                DecodeStep::Output { bytes, .. } => {
                    self.uncompressed
                        .update(bytes)
                        .map_err(LayerError::Integrity)?;
                    output(bytes).map_err(LayerError::Output)?;
                }
                DecodeStep::Progress { .. } => {}
                DecodeStep::NeedInput { .. } => break,
            }
        }
        self.decoder.finish().map_err(LayerError::Format)?;
        self.compressed.finish().map_err(LayerError::Integrity)?;
        self.uncompressed.finish().map_err(LayerError::Integrity)
    }
}

#[derive(Debug)]
pub enum LayerError<E> {
    Format(LayerFormatError),
    Integrity(VerifyError),
    Output(E),
}

impl<E: fmt::Display> fmt::Display for LayerError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => write!(formatter, "layer format error: {error}"),
            Self::Integrity(error) => write!(formatter, "layer integrity error: {error}"),
            Self::Output(error) => write!(formatter, "layer output error: {error}"),
        }
    }
}

#[derive(Debug)]
pub enum LayerFormatError {
    UnsupportedMediaType,
    EncodingDisabled(Encoding),
    DecoderStalled,
    #[cfg(feature = "gzip")]
    Gzip(gzip_zero::DecodeError),
    #[cfg(feature = "zstd")]
    Zstd(zstd_zero::DecodeError),
}

impl fmt::Display for LayerFormatError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMediaType => formatter.write_str("unsupported OCI layer media type"),
            Self::EncodingDisabled(encoding) => {
                write!(formatter, "{encoding:?} layer support is disabled")
            }
            Self::DecoderStalled => formatter.write_str("layer decoder stopped making progress"),
            #[cfg(feature = "gzip")]
            Self::Gzip(error) => write!(formatter, "{error}"),
            #[cfg(feature = "zstd")]
            Self::Zstd(error) => write!(formatter, "{error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    use super::{Decoder, LayerApplier, VerifiedDecoder};
    use crate::{
        digest::Digest,
        tar::{Archive, ArchiveBuffers, Entry, LayerEventSink, TransactionalLayerSink},
    };

    #[derive(Default)]
    struct TransactionSink {
        began: bool,
        committed: bool,
        aborted: bool,
    }

    impl LayerEventSink for TransactionSink {
        type Error = ();

        fn begin_entry(&mut self, _entry: Entry<'_>) -> Result<(), Self::Error> {
            Ok(())
        }

        fn entry_data(&mut self, _bytes: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn end_entry(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }

        fn whiteout(&mut self, _path: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }

        fn opaque_directory(&mut self, _path: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl TransactionalLayerSink for TransactionSink {
        fn begin_layer(&mut self) -> Result<(), Self::Error> {
            self.began = true;
            Ok(())
        }

        fn commit_layer(&mut self) -> Result<(), Self::Error> {
            self.committed = true;
            Ok(())
        }

        fn abort_layer(&mut self) {
            self.aborted = true;
        }
    }

    #[test]
    fn verifies_uncompressed_layers() {
        let bytes = b"tar bytes";
        let digest = Digest::from_bytes(Sha256::digest(bytes).into());
        let mut decoder = VerifiedDecoder::new(Decoder::tar(), digest, bytes.len() as u64, digest);
        let mut output = [0; 9];
        let mut length = 0;
        decoder
            .push(bytes, |chunk| {
                output[length..length + chunk.len()].copy_from_slice(chunk);
                length += chunk.len();
                Ok::<_, ()>(())
            })
            .unwrap();
        decoder.finish(|_| Ok::<_, ()>(())).unwrap();
        assert_eq!(&output, bytes);
    }

    #[test]
    fn commits_only_after_archive_and_integrity_finish() {
        let tar = [0; 1024];
        let digest = Digest::from_bytes(Sha256::digest(tar).into());
        let mut path = [0; 64];
        let mut link = [0; 64];
        let mut pax = [0; 64];
        let archive = Archive::new(ArchiveBuffers {
            path: &mut path,
            link: &mut link,
            pax: &mut pax,
        });
        let decoder = VerifiedDecoder::new(Decoder::tar(), digest, tar.len() as u64, digest);
        let mut applier = LayerApplier::new(decoder, archive);
        let mut sink = TransactionSink::default();
        applier.push(&tar, &mut sink).unwrap();
        assert!(sink.began);
        assert!(!sink.committed);
        applier.finish(&mut sink).unwrap();
        assert!(sink.committed);
        assert!(!sink.aborted);
    }

    #[test]
    fn aborts_when_integrity_fails() {
        let tar = [0; 1024];
        let diff_id = Digest::from_bytes(Sha256::digest(tar).into());
        let mut path = [0; 64];
        let mut link = [0; 64];
        let mut pax = [0; 64];
        let archive = Archive::new(ArchiveBuffers {
            path: &mut path,
            link: &mut link,
            pax: &mut pax,
        });
        let decoder = VerifiedDecoder::new(
            Decoder::tar(),
            Digest::from_bytes([1; 32]),
            tar.len() as u64,
            diff_id,
        );
        let mut applier = LayerApplier::new(decoder, archive);
        let mut sink = TransactionSink::default();
        applier.push(&tar, &mut sink).unwrap();
        assert!(applier.finish(&mut sink).is_err());
        assert!(sink.aborted);
        assert!(!sink.committed);
    }

    #[cfg(feature = "gzip")]
    #[test]
    fn verifies_gzip_layers() {
        const ENCODED: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9,
            0xc9, 0x07, 0x00, 0x86, 0xa6, 0x10, 0x36, 0x05, 0x00, 0x00, 0x00,
        ];
        let compressed = Digest::from_bytes(Sha256::digest(ENCODED).into());
        let diff_id = Digest::from_bytes(Sha256::digest(b"hello").into());
        let mut history = [0; gzip_zero::HISTORY_SIZE];
        let decoder = Decoder::gzip(gzip_zero::DecoderBuffers {
            history: &mut history,
        })
        .unwrap();
        let mut decoder = VerifiedDecoder::new(decoder, compressed, ENCODED.len() as u64, diff_id);
        let mut output = [0; 5];
        let mut length = 0;
        for byte in ENCODED {
            decoder
                .push(core::slice::from_ref(byte), |chunk| {
                    output[length..length + chunk.len()].copy_from_slice(chunk);
                    length += chunk.len();
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        decoder.finish(|_| Ok::<_, ()>(())).unwrap();
        assert_eq!(&output, b"hello");
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn verifies_zstd_layers() {
        const ENCODED: &[u8] = &[
            0x28, 0xb5, 0x2f, 0xfd, 0x20, 0x05, 0x29, 0, 0, b'h', b'e', b'l', b'l', b'o',
        ];
        let compressed = Digest::from_bytes(Sha256::digest(ENCODED).into());
        let diff_id = Digest::from_bytes(Sha256::digest(b"hello").into());
        let mut history = [0; 5];
        let mut block = [0; 5];
        let mut literals = [0; 5];
        let decoder = Decoder::zstd(zstd_zero::DecoderBuffers {
            history: &mut history,
            block: &mut block,
            literals: &mut literals,
        });
        let mut decoder = VerifiedDecoder::new(decoder, compressed, ENCODED.len() as u64, diff_id);
        let mut output = [0; 5];
        let mut length = 0;
        decoder
            .push(ENCODED, |chunk| {
                output[length..length + chunk.len()].copy_from_slice(chunk);
                length += chunk.len();
                Ok::<_, ()>(())
            })
            .unwrap();
        decoder.finish(|_| Ok::<_, ()>(())).unwrap();
        assert_eq!(&output, b"hello");
    }
}
