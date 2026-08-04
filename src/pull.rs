//! Callback-driven, allocation-free OCI graph traversal.

use core::fmt;

use crate::{
    digest::Digest,
    metadata::{Descriptor, Document, DocumentKind, ImageConfig, ImageIndex, ImageManifest},
    reference::{Reference, Selector},
};

pub const OCI_IMAGE_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
pub const DOCKER_IMAGE_CONFIG: &str = "application/vnd.docker.container.image.v1+json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestReference<'a> {
    Tag(&'a str),
    Digest(Digest),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    Skip,
    Pull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobAction {
    Skip,
    Fetch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobKind {
    Config,
    Layer { diff_id: Option<Digest> },
}

/// Receives streamed blob bytes from a [`Fetcher`].
pub trait BlobSink {
    fn chunk(&mut self, bytes: &[u8]);

    fn cancelled(&self) -> bool {
        false
    }
}

/// Supplies verified registry objects to the transport-independent pull walk.
///
/// Implementations must verify manifest digest references and each blob
/// descriptor's size and digest before returning success.
pub trait Fetcher {
    type Error;

    async fn manifest(
        &mut self,
        reference: ManifestReference<'_>,
        destination: &mut [u8],
    ) -> Result<usize, Self::Error>;

    async fn blob<S: BlobSink>(
        &mut self,
        descriptor: Descriptor<'_>,
        sink: &mut S,
    ) -> Result<(), Self::Error>;
}

/// Selects manifests and consumes pull events without retaining graph lists.
pub trait PullVisitor {
    type Error;

    fn index(&mut self, _index: ImageIndex<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn select_manifest(&mut self, _descriptor: Descriptor<'_>) -> Result<Selection, Self::Error> {
        Ok(Selection::Pull)
    }

    fn manifest(&mut self, _manifest: ImageManifest<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn subject(&mut self, _descriptor: Descriptor<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    fn begin_blob(
        &mut self,
        _kind: BlobKind,
        _descriptor: Descriptor<'_>,
    ) -> Result<BlobAction, Self::Error> {
        Ok(BlobAction::Fetch)
    }

    fn blob_data(&mut self, _kind: BlobKind, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn end_blob(&mut self, _kind: BlobKind) -> Result<(), Self::Error> {
        Ok(())
    }
}

pub struct PullBuffers<'a> {
    pub root_manifest: &'a mut [u8],
    pub child_manifest: &'a mut [u8],
    pub config: &'a mut [u8],
}

/// Pulls a root manifest, selects index children, fetches configs, and streams
/// selected layers in manifest order.
pub async fn pull<F, V>(
    fetcher: &mut F,
    reference: Reference<'_>,
    buffers: PullBuffers<'_>,
    visitor: &mut V,
) -> Result<(), PullError<F::Error, V::Error>>
where
    F: Fetcher,
    V: PullVisitor,
{
    let root_reference = match reference.selector() {
        Selector::Tag(tag) => ManifestReference::Tag(tag),
        Selector::Digest(digest) => ManifestReference::Digest(digest),
    };
    let root_length = fetcher
        .manifest(root_reference, buffers.root_manifest)
        .await
        .map_err(PullError::Fetch)?;
    if root_length > buffers.root_manifest.len() {
        return Err(PullError::BufferContract);
    }
    let root =
        Document::parse(&buffers.root_manifest[..root_length]).map_err(PullError::Metadata)?;
    match root.kind() {
        DocumentKind::Manifest => {
            pull_manifest(
                fetcher,
                root.manifest().map_err(PullError::Metadata)?,
                buffers.config,
                visitor,
            )
            .await
        }
        DocumentKind::Index => {
            let index = root.index().map_err(PullError::Metadata)?;
            visitor.index(index).map_err(PullError::Visitor)?;
            for descriptor in index.manifests().map_err(PullError::Metadata)? {
                let descriptor = descriptor.map_err(PullError::Metadata)?;
                if visitor
                    .select_manifest(descriptor)
                    .map_err(PullError::Visitor)?
                    == Selection::Skip
                {
                    continue;
                }
                let length = fetcher
                    .manifest(
                        ManifestReference::Digest(descriptor.digest()),
                        buffers.child_manifest,
                    )
                    .await
                    .map_err(PullError::Fetch)?;
                if length > buffers.child_manifest.len() {
                    return Err(PullError::BufferContract);
                }
                let child = Document::parse(&buffers.child_manifest[..length])
                    .map_err(PullError::Metadata)?;
                if child.kind() != DocumentKind::Manifest {
                    return Err(PullError::NestedIndex);
                }
                pull_manifest(
                    fetcher,
                    child.manifest().map_err(PullError::Metadata)?,
                    buffers.config,
                    visitor,
                )
                .await?;
            }
            Ok(())
        }
    }
}

async fn pull_manifest<F, V>(
    fetcher: &mut F,
    manifest: ImageManifest<'_>,
    config_buffer: &mut [u8],
    visitor: &mut V,
) -> Result<(), PullError<F::Error, V::Error>>
where
    F: Fetcher,
    V: PullVisitor,
{
    visitor.manifest(manifest).map_err(PullError::Visitor)?;
    if let Some(subject) = manifest.subject().map_err(PullError::Metadata)? {
        visitor.subject(subject).map_err(PullError::Visitor)?;
    }

    let config = manifest.config().map_err(PullError::Metadata)?;
    let mut config_sink = BufferSink::new(config_buffer);
    fetcher
        .blob(config, &mut config_sink)
        .await
        .map_err(PullError::Fetch)?;
    let config_length = config_sink
        .finish()
        .map_err(|_| PullError::ConfigTooLarge)?;
    let action = visitor
        .begin_blob(BlobKind::Config, config)
        .map_err(PullError::Visitor)?;
    if action == BlobAction::Fetch {
        visitor
            .blob_data(BlobKind::Config, &config_buffer[..config_length])
            .map_err(PullError::Visitor)?;
        visitor
            .end_blob(BlobKind::Config)
            .map_err(PullError::Visitor)?;
    }

    let config_media_type = config.media_type();
    let image_config = config_media_type.decoded_eq_ascii(OCI_IMAGE_CONFIG)
        || config_media_type.decoded_eq_ascii(DOCKER_IMAGE_CONFIG);
    let mut diff_ids = if image_config {
        let config_document =
            ImageConfig::parse(&config_buffer[..config_length]).map_err(PullError::Metadata)?;
        let diff_ids = config_document.diff_ids().map_err(PullError::Metadata)?;
        if diff_ids.is_none() {
            return Err(PullError::MissingDiffIds);
        }
        diff_ids
    } else {
        // Artifact configs are media-type-defined blobs and need not be JSON.
        None
    };

    for descriptor in manifest.layers().map_err(PullError::Metadata)? {
        let descriptor = descriptor.map_err(PullError::Metadata)?;
        let diff_id = match diff_ids.as_mut() {
            Some(ids) => Some(
                ids.next()
                    .ok_or(PullError::DiffIdCount)?
                    .map_err(PullError::Metadata)?,
            ),
            None => None,
        };
        let kind = BlobKind::Layer { diff_id };
        let action = visitor
            .begin_blob(kind, descriptor)
            .map_err(PullError::Visitor)?;
        if action == BlobAction::Skip {
            continue;
        }
        let mut sink = VisitorSink {
            visitor,
            kind,
            error: None,
        };
        fetcher
            .blob(descriptor, &mut sink)
            .await
            .map_err(PullError::Fetch)?;
        if let Some(error) = sink.error {
            return Err(PullError::Visitor(error));
        }
        visitor.end_blob(kind).map_err(PullError::Visitor)?;
    }
    if diff_ids.as_mut().and_then(Iterator::next).is_some() {
        return Err(PullError::DiffIdCount);
    }
    Ok(())
}

struct BufferSink<'a> {
    buffer: &'a mut [u8],
    length: usize,
    overflow: bool,
}

impl<'a> BufferSink<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            length: 0,
            overflow: false,
        }
    }

    fn finish(&self) -> Result<usize, ()> {
        (!self.overflow).then_some(self.length).ok_or(())
    }
}

impl BlobSink for BufferSink<'_> {
    fn chunk(&mut self, bytes: &[u8]) {
        let Some(end) = self.length.checked_add(bytes.len()) else {
            self.overflow = true;
            return;
        };
        let Some(output) = self.buffer.get_mut(self.length..end) else {
            self.overflow = true;
            return;
        };
        output.copy_from_slice(bytes);
        self.length = end;
    }

    fn cancelled(&self) -> bool {
        self.overflow
    }
}

struct VisitorSink<'a, V: PullVisitor> {
    visitor: &'a mut V,
    kind: BlobKind,
    error: Option<V::Error>,
}

impl<V: PullVisitor> BlobSink for VisitorSink<'_, V> {
    fn chunk(&mut self, bytes: &[u8]) {
        if self.error.is_none() {
            self.error = self.visitor.blob_data(self.kind, bytes).err();
        }
    }

    fn cancelled(&self) -> bool {
        self.error.is_some()
    }
}

#[derive(Debug)]
pub enum PullError<F, V> {
    Fetch(F),
    Visitor(V),
    Metadata(crate::metadata::MetadataError),
    ConfigTooLarge,
    MissingDiffIds,
    DiffIdCount,
    NestedIndex,
    BufferContract,
}

impl<F: fmt::Display, V: fmt::Display> fmt::Display for PullError<F, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(error) => write!(formatter, "registry fetch failed: {error}"),
            Self::Visitor(error) => write!(formatter, "pull visitor failed: {error}"),
            Self::Metadata(error) => write!(formatter, "OCI metadata failed: {error}"),
            Self::ConfigTooLarge => formatter.write_str("image config exceeds its caller buffer"),
            Self::MissingDiffIds => formatter.write_str("image config is missing rootfs.diff_ids"),
            Self::DiffIdCount => formatter.write_str("layer and diff_id counts differ"),
            Self::NestedIndex => formatter.write_str("nested OCI indexes exceed this pull frame"),
            Self::BufferContract => formatter.write_str("fetcher returned an out-of-range length"),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{future::Future, task::Poll};
    use std::{sync::Arc, task::Wake};

    use super::{
        pull, BlobAction, BlobKind, BlobSink, Fetcher, ManifestReference, PullBuffers, PullVisitor,
    };
    use crate::{metadata::Descriptor, reference::Reference};

    const MANIFEST: &[u8] = br#"{
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "artifactType": "application/example",
        "config": {
            "mediaType": "application/vnd.example.config",
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "size": 8
        },
        "layers": [{
            "mediaType": "application/vnd.example.payload",
            "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "size": 5
        }]
    }"#;

    const ESCAPED_MEDIA_TYPE_MANIFEST: &[u8] = br#"{
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application\/vnd.oci.image.config.v1+json",
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "size": 115
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "size": 5
        }]
    }"#;

    const IMAGE_CONFIG: &[u8] = br#"{"rootfs":{"type":"layers","diff_ids":["sha256:2222222222222222222222222222222222222222222222222222222222222222"]}}"#;

    struct MockFetcher;

    impl Fetcher for MockFetcher {
        type Error = ();

        async fn manifest(
            &mut self,
            _reference: ManifestReference<'_>,
            destination: &mut [u8],
        ) -> Result<usize, Self::Error> {
            destination[..MANIFEST.len()].copy_from_slice(MANIFEST);
            Ok(MANIFEST.len())
        }

        async fn blob<S: BlobSink>(
            &mut self,
            descriptor: Descriptor<'_>,
            sink: &mut S,
        ) -> Result<(), Self::Error> {
            if descriptor.media_type().as_str() == Some("application/vnd.example.config") {
                sink.chunk(b"not json");
            } else {
                sink.chunk(b"layer");
            }
            Ok(())
        }
    }

    struct EscapedMediaTypeFetcher;

    impl Fetcher for EscapedMediaTypeFetcher {
        type Error = ();

        async fn manifest(
            &mut self,
            _reference: ManifestReference<'_>,
            destination: &mut [u8],
        ) -> Result<usize, Self::Error> {
            destination[..ESCAPED_MEDIA_TYPE_MANIFEST.len()]
                .copy_from_slice(ESCAPED_MEDIA_TYPE_MANIFEST);
            Ok(ESCAPED_MEDIA_TYPE_MANIFEST.len())
        }

        async fn blob<S: BlobSink>(
            &mut self,
            descriptor: Descriptor<'_>,
            sink: &mut S,
        ) -> Result<(), Self::Error> {
            if descriptor.media_type().encoded().contains("\\/") {
                sink.chunk(IMAGE_CONFIG);
            } else {
                sink.chunk(b"layer");
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct Visitor {
        config_bytes: usize,
        layer_bytes: usize,
        layer_diff_id_was_none: bool,
        layer_diff_id_was_some: bool,
    }

    impl PullVisitor for Visitor {
        type Error = ();

        fn begin_blob(
            &mut self,
            kind: BlobKind,
            _descriptor: Descriptor<'_>,
        ) -> Result<BlobAction, Self::Error> {
            if let BlobKind::Layer { diff_id } = kind {
                self.layer_diff_id_was_none = diff_id.is_none();
                self.layer_diff_id_was_some = diff_id.is_some();
            }
            Ok(BlobAction::Fetch)
        }

        fn blob_data(&mut self, kind: BlobKind, bytes: &[u8]) -> Result<(), Self::Error> {
            match kind {
                BlobKind::Config => self.config_bytes += bytes.len(),
                BlobKind::Layer { .. } => self.layer_bytes += bytes.len(),
            }
            Ok(())
        }
    }

    #[test]
    fn pulls_non_json_artifact_configs() {
        let mut fetcher = MockFetcher;
        let mut visitor = Visitor::default();
        let mut root = [0; 1024];
        let mut child = [0; 1024];
        let mut config = [0; 64];
        let future = pull(
            &mut fetcher,
            Reference::parse("oci://example.com/artifact:latest").unwrap(),
            PullBuffers {
                root_manifest: &mut root,
                child_manifest: &mut child,
                config: &mut config,
            },
            &mut visitor,
        );
        assert!(block_on_ready(future).is_ok());
        assert_eq!(visitor.config_bytes, 8);
        assert_eq!(visitor.layer_bytes, 5);
        assert!(visitor.layer_diff_id_was_none);
    }

    #[test]
    fn recognizes_escaped_image_config_media_types() {
        let mut fetcher = EscapedMediaTypeFetcher;
        let mut visitor = Visitor::default();
        let mut root = [0; 1024];
        let mut child = [0; 1024];
        let mut config = [0; 256];
        let future = pull(
            &mut fetcher,
            Reference::parse("oci://example.com/image:latest").unwrap(),
            PullBuffers {
                root_manifest: &mut root,
                child_manifest: &mut child,
                config: &mut config,
            },
            &mut visitor,
        );
        assert!(block_on_ready(future).is_ok());
        assert!(visitor.layer_diff_id_was_some);
    }

    fn block_on_ready<F: Future>(future: F) -> F::Output {
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
        }

        let waker = std::task::Waker::from(Arc::new(NoopWake));
        let mut context = core::task::Context::from_waker(&waker);
        let mut future = core::pin::pin!(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("in-memory pull unexpectedly yielded"),
        }
    }
}
