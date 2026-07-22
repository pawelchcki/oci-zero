//! Allocation-free borrowed views over OCI JSON metadata.

use core::fmt;

pub use crate::json::{JsonError, JsonString};
use crate::{
    digest::{Digest, DigestError},
    json::{ArrayIter, Object, ObjectIter, Value},
};

pub const OCI_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
pub const DOCKER_INDEX_MEDIA_TYPE: &str =
    "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentKind {
    Index,
    Manifest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Document<'a> {
    object: Object<'a>,
    kind: DocumentKind,
}

impl<'a> Document<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MetadataError> {
        let object = Value::parse_document(bytes)?.object()?;
        let schema = object.required("schemaVersion")?.u64()?;
        if schema != 2 {
            return Err(MetadataError::UnsupportedSchema(schema));
        }
        let has_manifests = object.get("manifests")?.is_some();
        let has_config = object.get("config")?.is_some();
        let has_layers = object.get("layers")?.is_some();
        let kind = match (has_manifests, has_config, has_layers) {
            (true, false, false) => DocumentKind::Index,
            (false, true, true) => DocumentKind::Manifest,
            _ => return Err(MetadataError::UnknownDocument),
        };
        Ok(Self { object, kind })
    }

    pub const fn kind(&self) -> DocumentKind {
        self.kind
    }

    pub fn index(self) -> Result<ImageIndex<'a>, MetadataError> {
        if self.kind != DocumentKind::Index {
            return Err(MetadataError::WrongDocumentKind);
        }
        Ok(ImageIndex {
            object: self.object,
        })
    }

    pub fn manifest(self) -> Result<ImageManifest<'a>, MetadataError> {
        if self.kind != DocumentKind::Manifest {
            return Err(MetadataError::WrongDocumentKind);
        }
        Ok(ImageManifest {
            object: self.object,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageIndex<'a> {
    object: Object<'a>,
}

impl<'a> ImageIndex<'a> {
    pub fn media_type(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "mediaType")
    }

    pub fn artifact_type(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "artifactType")
    }

    pub fn subject(self) -> Result<Option<Descriptor<'a>>, MetadataError> {
        self.object
            .get("subject")?
            .map(Descriptor::parse)
            .transpose()
    }

    pub fn manifests(self) -> Result<DescriptorIter<'a>, MetadataError> {
        Ok(DescriptorIter {
            inner: self.object.required("manifests")?.array()?.iter(),
        })
    }

    pub fn annotations(self) -> Result<AnnotationIter<'a>, MetadataError> {
        annotations(self.object)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageManifest<'a> {
    object: Object<'a>,
}

impl<'a> ImageManifest<'a> {
    pub fn media_type(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "mediaType")
    }

    pub fn artifact_type(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "artifactType")
    }

    pub fn subject(self) -> Result<Option<Descriptor<'a>>, MetadataError> {
        self.object
            .get("subject")?
            .map(Descriptor::parse)
            .transpose()
    }

    pub fn config(self) -> Result<Descriptor<'a>, MetadataError> {
        Descriptor::parse(self.object.required("config")?)
    }

    pub fn layers(self) -> Result<DescriptorIter<'a>, MetadataError> {
        Ok(DescriptorIter {
            inner: self.object.required("layers")?.array()?.iter(),
        })
    }

    pub fn annotations(self) -> Result<AnnotationIter<'a>, MetadataError> {
        annotations(self.object)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Descriptor<'a> {
    object: Object<'a>,
    media_type: JsonString<'a>,
    digest: Digest,
    size: u64,
}

impl<'a> Descriptor<'a> {
    fn parse(value: Value<'a>) -> Result<Self, MetadataError> {
        let object = value.object()?;
        let media_type = object.required("mediaType")?.string()?;
        let digest_string = object.required("digest")?.string()?;
        let mut digest_buffer = [0; 71];
        let digest = Digest::parse(digest_string.decode_into(&mut digest_buffer)?)?;
        let size = object.required("size")?.u64()?;
        Ok(Self {
            object,
            media_type,
            digest,
            size,
        })
    }

    pub const fn media_type(&self) -> JsonString<'a> {
        self.media_type
    }

    pub const fn digest(&self) -> Digest {
        self.digest
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub fn artifact_type(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "artifactType")
    }

    pub fn platform(self) -> Result<Option<Platform<'a>>, MetadataError> {
        self.object
            .get("platform")?
            .map(Platform::parse)
            .transpose()
    }

    pub fn annotations(self) -> Result<AnnotationIter<'a>, MetadataError> {
        annotations(self.object)
    }

    pub fn urls(self) -> Result<StringIter<'a>, MetadataError> {
        strings(self.object, "urls")
    }

    pub fn data(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "data")
    }

    /// Decodes an optional inline descriptor payload into caller-owned storage.
    ///
    /// The same buffer is first used to unescape the JSON string and is then
    /// decoded in place, so it must hold the base64 text rather than only the
    /// smaller decoded payload.
    pub fn decode_data(self, buffer: &mut [u8]) -> Result<Option<&[u8]>, MetadataError> {
        let Some(data) = self.data()? else {
            return Ok(None);
        };
        let encoded_length = data.decode_into(buffer)?.len();
        let decoded_length = decode_base64_in_place(buffer, encoded_length)?;
        Ok(Some(&buffer[..decoded_length]))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Platform<'a> {
    object: Object<'a>,
    architecture: JsonString<'a>,
    os: JsonString<'a>,
}

impl<'a> Platform<'a> {
    fn parse(value: Value<'a>) -> Result<Self, MetadataError> {
        let object = value.object()?;
        Ok(Self {
            object,
            architecture: object.required("architecture")?.string()?,
            os: object.required("os")?.string()?,
        })
    }

    pub const fn architecture(&self) -> JsonString<'a> {
        self.architecture
    }

    pub const fn os(&self) -> JsonString<'a> {
        self.os
    }

    pub fn variant(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "variant")
    }

    pub fn os_version(self) -> Result<Option<JsonString<'a>>, MetadataError> {
        optional_string(self.object, "os.version")
    }

    pub fn os_features(self) -> Result<StringIter<'a>, MetadataError> {
        strings(self.object, "os.features")
    }
}

pub struct DescriptorIter<'a> {
    inner: ArrayIter<'a>,
}

impl<'a> Iterator for DescriptorIter<'a> {
    type Item = Result<Descriptor<'a>, MetadataError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|value| Descriptor::parse(value?))
    }
}

pub struct AnnotationIter<'a> {
    inner: Option<ObjectIter<'a>>,
}

impl<'a> Iterator for AnnotationIter<'a> {
    type Item = Result<(JsonString<'a>, JsonString<'a>), MetadataError>;

    fn next(&mut self) -> Option<Self::Item> {
        let inner = self.inner.as_mut()?;
        inner.next().map(|member| {
            let (key, value) = member?;
            Ok((key, value.string()?))
        })
    }
}

pub struct StringIter<'a> {
    inner: Option<ArrayIter<'a>>,
}

impl<'a> Iterator for StringIter<'a> {
    type Item = Result<JsonString<'a>, MetadataError>;

    fn next(&mut self) -> Option<Self::Item> {
        let inner = self.inner.as_mut()?;
        inner.next().map(|value| Ok(value?.string()?))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageConfig<'a> {
    object: Object<'a>,
}

impl<'a> ImageConfig<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MetadataError> {
        Ok(Self {
            object: Value::parse_document(bytes)?.object()?,
        })
    }

    pub fn diff_ids(self) -> Result<Option<DigestIter<'a>>, MetadataError> {
        let Some(rootfs) = self.object.get("rootfs")? else {
            return Ok(None);
        };
        let rootfs = rootfs.object()?;
        let Some(diff_ids) = rootfs.get("diff_ids")? else {
            return Ok(None);
        };
        Ok(Some(DigestIter {
            inner: diff_ids.array()?.iter(),
        }))
    }
}

pub struct DigestIter<'a> {
    inner: ArrayIter<'a>,
}

impl Iterator for DigestIter<'_> {
    type Item = Result<Digest, MetadataError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|value| {
            let string = value?.string()?;
            let mut buffer = [0; 71];
            Ok(Digest::parse(string.decode_into(&mut buffer)?)?)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TagList<'a> {
    object: Object<'a>,
}

impl<'a> TagList<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MetadataError> {
        Ok(Self {
            object: Value::parse_document(bytes)?.object()?,
        })
    }

    pub fn name(self) -> Result<JsonString<'a>, MetadataError> {
        Ok(self.object.required("name")?.string()?)
    }

    pub fn tags(self) -> Result<StringIter<'a>, MetadataError> {
        strings(self.object, "tags")
    }
}

/// A Docker Distribution registry catalog response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Catalog<'a> {
    object: Object<'a>,
}

impl<'a> Catalog<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, MetadataError> {
        Ok(Self {
            object: Value::parse_document(bytes)?.object()?,
        })
    }

    pub fn repositories(self) -> Result<StringIter<'a>, MetadataError> {
        strings(self.object, "repositories")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataError {
    Json(JsonError),
    Digest(DigestError),
    UnsupportedSchema(u64),
    UnknownDocument,
    WrongDocumentKind,
    InvalidBase64,
}

impl fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid OCI JSON: {error}"),
            Self::Digest(error) => write!(formatter, "invalid descriptor digest: {error}"),
            Self::UnsupportedSchema(schema) => {
                write!(formatter, "unsupported OCI schema version {schema}")
            }
            Self::UnknownDocument => formatter.write_str("unknown OCI document shape"),
            Self::WrongDocumentKind => formatter.write_str("unexpected OCI document kind"),
            Self::InvalidBase64 => formatter.write_str("invalid inline descriptor base64 data"),
        }
    }
}

fn decode_base64_in_place(bytes: &mut [u8], encoded_length: usize) -> Result<usize, MetadataError> {
    if encoded_length % 4 != 0 {
        return Err(MetadataError::InvalidBase64);
    }
    let mut read = 0;
    let mut write = 0;
    while read < encoded_length {
        let last = read + 4 == encoded_length;
        let first = base64_value(bytes[read]).ok_or(MetadataError::InvalidBase64)?;
        let second = base64_value(bytes[read + 1]).ok_or(MetadataError::InvalidBase64)?;
        let third_padding = bytes[read + 2] == b'=';
        let fourth_padding = bytes[read + 3] == b'=';
        if third_padding {
            if !last || !fourth_padding || second & 0x0f != 0 {
                return Err(MetadataError::InvalidBase64);
            }
            bytes[write] = first << 2 | second >> 4;
            write += 1;
        } else {
            let third = base64_value(bytes[read + 2]).ok_or(MetadataError::InvalidBase64)?;
            bytes[write] = first << 2 | second >> 4;
            bytes[write + 1] = second << 4 | third >> 2;
            write += 2;
            if fourth_padding {
                if !last || third & 0x03 != 0 {
                    return Err(MetadataError::InvalidBase64);
                }
            } else {
                let fourth = base64_value(bytes[read + 3]).ok_or(MetadataError::InvalidBase64)?;
                bytes[write] = third << 6 | fourth;
                write += 1;
            }
        }
        read += 4;
    }
    Ok(write)
}

const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

impl From<JsonError> for MetadataError {
    fn from(error: JsonError) -> Self {
        Self::Json(error)
    }
}

impl From<DigestError> for MetadataError {
    fn from(error: DigestError) -> Self {
        Self::Digest(error)
    }
}

fn optional_string<'a>(
    object: Object<'a>,
    name: &'static str,
) -> Result<Option<JsonString<'a>>, MetadataError> {
    Ok(object.get(name)?.map(Value::string).transpose()?)
}

fn annotations<'a>(object: Object<'a>) -> Result<AnnotationIter<'a>, MetadataError> {
    let inner = object
        .get("annotations")?
        .filter(|value| !value.is_null())
        .map(Value::object)
        .transpose()?
        .map(Object::iter);
    Ok(AnnotationIter { inner })
}

fn strings<'a>(object: Object<'a>, name: &'static str) -> Result<StringIter<'a>, MetadataError> {
    let inner = object
        .get(name)?
        .filter(|value| !value.is_null())
        .map(Value::array)
        .transpose()?
        .map(|array| array.iter());
    Ok(StringIter { inner })
}

#[cfg(test)]
mod tests {
    use std::{format, string::ToString, vec::Vec};

    use super::{Catalog, Document, DocumentKind, ImageConfig, MetadataError, TagList};

    const DIGEST: &str = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parses_index_descriptors_lazily() {
        let json = format!(
            r#"{{"schemaVersion":2,"artifactType":"application/example","subject":{{"mediaType":"application/vnd.oci.image.manifest.v1+json","size":3,"digest":"{DIGEST}"}},"unknown":{{"large":[1,2,3]}},"manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","size":3,"digest":"{DIGEST}","platform":{{"architecture":"amd64","os":"linux"}},"annotations":{{"title":"value\n"}}}}]}}"#
        );
        let document = Document::parse(json.as_bytes()).unwrap();
        assert_eq!(document.kind(), DocumentKind::Index);
        let index = document.index().unwrap();
        assert_eq!(
            index.artifact_type().unwrap().unwrap().as_str(),
            Some("application/example")
        );
        assert_eq!(
            index.subject().unwrap().unwrap().digest().to_string(),
            DIGEST
        );
        let descriptor = document
            .index()
            .unwrap()
            .manifests()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(descriptor.size(), 3);
        let platform = descriptor.platform().unwrap().unwrap();
        assert_eq!(platform.os().as_str(), Some("linux"));
        let (_, title) = descriptor.annotations().unwrap().next().unwrap().unwrap();
        let mut decoded = [0; 16];
        assert_eq!(title.decode_into(&mut decoded).unwrap(), "value\n");
    }

    #[test]
    fn parses_manifest_and_diff_ids() {
        let json = format!(
            r#"{{"schemaVersion":2,"config":{{"mediaType":"x","size":3,"digest":"{DIGEST}"}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","size":3,"digest":"{DIGEST}"}}]}}"#
        );
        let manifest = Document::parse(json.as_bytes())
            .unwrap()
            .manifest()
            .unwrap();
        assert_eq!(manifest.config().unwrap().size(), 3);
        assert_eq!(manifest.layers().unwrap().count(), 1);

        let config = format!(r#"{{"rootfs":{{"type":"layers","diff_ids":["{DIGEST}"]}}}}"#);
        assert_eq!(
            ImageConfig::parse(config.as_bytes())
                .unwrap()
                .diff_ids()
                .unwrap()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .to_string(),
            DIGEST
        );
    }

    #[test]
    fn parses_catalogs_and_tag_lists() {
        let catalog = Catalog::parse(br#"{"repositories":["a","team/b"]}"#).unwrap();
        let repositories = catalog
            .repositories()
            .unwrap()
            .map(|value| value.unwrap().as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(repositories, ["a", "team/b"]);

        let tags = TagList::parse(br#"{"name":"team/b","tags":["latest","v1"]}"#).unwrap();
        assert_eq!(tags.name().unwrap().as_str(), Some("team/b"));
        assert_eq!(tags.tags().unwrap().count(), 2);

        let empty = TagList::parse(br#"{"name":"empty","tags":null}"#).unwrap();
        assert_eq!(empty.tags().unwrap().count(), 0);
    }

    #[test]
    fn decodes_inline_descriptor_data_without_allocating() {
        let json = format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"x","size":5,"digest":"{DIGEST}","data":"aGVs\u0062G8="}}]}}"#
        );
        let descriptor = Document::parse(json.as_bytes())
            .unwrap()
            .index()
            .unwrap()
            .manifests()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let mut buffer = [0; 32];
        assert_eq!(
            descriptor.decode_data(&mut buffer).unwrap(),
            Some(b"hello".as_slice())
        );

        let invalid = format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"x","size":1,"digest":"{DIGEST}","data":"A==="}}]}}"#
        );
        let descriptor = Document::parse(invalid.as_bytes())
            .unwrap()
            .index()
            .unwrap()
            .manifests()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            descriptor.decode_data(&mut buffer),
            Err(MetadataError::InvalidBase64)
        );
    }
}
