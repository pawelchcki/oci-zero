use std::{borrow::Cow, string::String, vec, vec::Vec};

use js_sys::{Array, Function, Promise, Reflect, Uint8Array};
use oci_zero::{
    compression::{
        gzip::{self, DecoderBuffers as GzipBuffers},
        zstd::{self, DecoderBuffers as ZstdBuffers, HeaderStatus, StreamHeader, MAX_BLOCK_SIZE},
    },
    digest::Digest,
    layer::{encoding, Decoder, Encoding, LayerApplier, VerifiedDecoder, VerifiedEntryExtractor},
    metadata::{Catalog, Descriptor, Document, DocumentKind, ImageConfig, JsonString, TagList},
    reference::Repository,
    tar::{
        Archive, ArchiveBuffers, Entry, EntryKind, LayerEventSink, TarWriter,
        TransactionalLayerSink,
    },
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

const MAX_ZSTD_WINDOW: usize = 256 * 1024 * 1024;
const MAX_FILE_ENTRIES: usize = 200_000;
const MAX_EXTRACTED_FILE: usize = 256 * 1024 * 1024;
const MAX_ARCHIVE: usize = 256 * 1024 * 1024;

#[derive(Serialize)]
struct NormalizedRepository {
    scheme: String,
    registry: String,
    repository: String,
    canonical: String,
}

#[wasm_bindgen]
pub fn normalize_repository(input: &str) -> Result<JsValue, JsValue> {
    let input = input.trim().trim_end_matches('/');
    let (scheme, value) = if let Some(value) = input.strip_prefix("oci://") {
        ("https", value)
    } else if let Some(value) = input.strip_prefix("https://") {
        ("https", value)
    } else if let Some(value) = input.strip_prefix("http://") {
        ("http", value)
    } else {
        ("https", input)
    };
    let (registry, mut repository) = value.split_once('/').ok_or_else(|| {
        js_error("enter a registry and repository, for example docker.io/library/alpine")
    })?;
    let registry = match registry.to_ascii_lowercase().as_str() {
        "docker.io" | "index.docker.io" => "registry-1.docker.io".to_owned(),
        _ => registry.to_ascii_lowercase(),
    };
    let owned_repository;
    if registry == "registry-1.docker.io" && !repository.contains('/') {
        owned_repository = format!("library/{repository}");
        repository = &owned_repository;
    }
    let canonical = format!("oci://{registry}/{repository}");
    Repository::parse(&canonical).map_err(js_display)?;
    to_js(&NormalizedRepository {
        scheme: scheme.to_owned(),
        registry,
        repository: repository.to_owned(),
        canonical,
    })
}

#[wasm_bindgen]
pub fn parse_catalog(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let catalog = Catalog::parse(bytes).map_err(js_display)?;
    let repositories = catalog
        .repositories()
        .map_err(js_display)?
        .map(|value| owned_json(value.map_err(js_display)?))
        .collect::<Result<Vec<_>, _>>()?;
    to_js(&repositories)
}

#[derive(Serialize)]
struct TagsView {
    name: String,
    tags: Vec<String>,
}

#[wasm_bindgen]
pub fn parse_tags(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let list = TagList::parse(bytes).map_err(js_display)?;
    let name = owned_json(list.name().map_err(js_display)?)?;
    let tags = list
        .tags()
        .map_err(js_display)?
        .map(|value| owned_json(value.map_err(js_display)?))
        .collect::<Result<Vec<_>, _>>()?;
    to_js(&TagsView { name, tags })
}

#[derive(Serialize)]
struct DocumentView {
    kind: &'static str,
    media_type: Option<String>,
    artifact_type: Option<String>,
    annotations: Vec<(String, String)>,
    subject: Option<DescriptorView>,
    config: Option<DescriptorView>,
    layers: Vec<DescriptorView>,
    manifests: Vec<DescriptorView>,
}

#[derive(Serialize)]
struct DescriptorView {
    media_type: String,
    digest: String,
    size: u64,
    artifact_type: Option<String>,
    platform: Option<PlatformView>,
    annotations: Vec<(String, String)>,
}

#[derive(Serialize)]
struct PlatformView {
    os: String,
    architecture: String,
    variant: Option<String>,
    os_version: Option<String>,
}

#[wasm_bindgen]
pub fn parse_document(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let document = Document::parse(bytes).map_err(js_display)?;
    let view = match document.kind() {
        DocumentKind::Index => {
            let index = document.index().map_err(js_display)?;
            DocumentView {
                kind: "index",
                media_type: optional_json(index.media_type().map_err(js_display)?)?,
                artifact_type: optional_json(index.artifact_type().map_err(js_display)?)?,
                annotations: owned_annotations(index.annotations().map_err(js_display)?)?,
                subject: index
                    .subject()
                    .map_err(js_display)?
                    .map(descriptor_view)
                    .transpose()?,
                config: None,
                layers: Vec::new(),
                manifests: index
                    .manifests()
                    .map_err(js_display)?
                    .map(|descriptor| descriptor_view(descriptor.map_err(js_display)?))
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        DocumentKind::Manifest => {
            let manifest = document.manifest().map_err(js_display)?;
            DocumentView {
                kind: "manifest",
                media_type: optional_json(manifest.media_type().map_err(js_display)?)?,
                artifact_type: optional_json(manifest.artifact_type().map_err(js_display)?)?,
                annotations: owned_annotations(manifest.annotations().map_err(js_display)?)?,
                subject: manifest
                    .subject()
                    .map_err(js_display)?
                    .map(descriptor_view)
                    .transpose()?,
                config: Some(descriptor_view(manifest.config().map_err(js_display)?)?),
                layers: manifest
                    .layers()
                    .map_err(js_display)?
                    .map(|descriptor| descriptor_view(descriptor.map_err(js_display)?))
                    .collect::<Result<Vec<_>, _>>()?,
                manifests: Vec::new(),
            }
        }
    };
    to_js(&view)
}

#[wasm_bindgen]
pub fn parse_diff_ids(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let config = ImageConfig::parse(bytes).map_err(js_display)?;
    let diff_ids = config
        .diff_ids()
        .map_err(js_display)?
        .ok_or_else(|| js_error("image config does not contain rootfs.diff_ids"))?
        .map(|digest| digest.map(|value| value.to_string()).map_err(js_display))
        .collect::<Result<Vec<_>, _>>()?;
    to_js(&diff_ids)
}

/// Returns the archive encoding understood by the browser, without requiring a
/// blob download. Unknown vendor payloads can therefore be shown as skipped.
#[wasm_bindgen]
pub fn layer_encoding(media_type: &str) -> Option<String> {
    browser_encoding(media_type).ok().map(|encoding| {
        match encoding {
            Encoding::Tar => "tar",
            Encoding::Gzip => "gzip",
            Encoding::Zstd => "zstd",
        }
        .to_owned()
    })
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LayerEvent {
    Entry {
        path: String,
        kind: &'static str,
        size: u64,
        mode: u64,
        uid: u64,
        gid: u64,
        mtime: u64,
        link_target: Option<String>,
    },
    Whiteout {
        path: String,
    },
    OpaqueDirectory {
        path: String,
    },
}

#[derive(Default)]
struct EventSink {
    events: Vec<LayerEvent>,
    event_count: usize,
    active: bool,
}

impl LayerEventSink for EventSink {
    type Error = WebError;

    fn begin_entry(&mut self, entry: Entry<'_>) -> Result<(), Self::Error> {
        self.record_event()?;
        self.events.push(LayerEvent::Entry {
            path: path_text(entry.path),
            kind: entry_kind(entry.kind),
            size: entry.size,
            mode: entry.mode,
            uid: entry.uid,
            gid: entry.gid,
            mtime: entry.mtime,
            link_target: entry.link_target.map(path_text),
        });
        Ok(())
    }

    fn entry_data(&mut self, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn end_entry(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn whiteout(&mut self, path: &[u8]) -> Result<(), Self::Error> {
        self.record_event()?;
        self.events.push(LayerEvent::Whiteout {
            path: path_text(path),
        });
        Ok(())
    }

    fn opaque_directory(&mut self, path: &[u8]) -> Result<(), Self::Error> {
        self.record_event()?;
        self.events.push(LayerEvent::OpaqueDirectory {
            path: path_text(path),
        });
        Ok(())
    }
}

impl EventSink {
    fn record_event(&mut self) -> Result<(), WebError> {
        if self.event_count >= MAX_FILE_ENTRIES {
            return Err(WebError(
                "layer contains more than 200000 filesystem events".to_owned(),
            ));
        }
        self.event_count += 1;
        Ok(())
    }
}

impl TransactionalLayerSink for EventSink {
    fn begin_layer(&mut self) -> Result<(), Self::Error> {
        if self.active {
            return Err(WebError("layer transaction is already active".to_owned()));
        }
        self.active = true;
        Ok(())
    }

    fn commit_layer(&mut self) -> Result<(), Self::Error> {
        if !self.active {
            return Err(WebError("layer transaction is not active".to_owned()));
        }
        self.active = false;
        Ok(())
    }

    fn abort_layer(&mut self) {
        self.active = false;
        self.events.clear();
    }
}

#[wasm_bindgen]
pub async fn scan_layer_stream(
    media_type: &str,
    compressed_digest: &str,
    compressed_size: u64,
    diff_id: Option<String>,
    next_chunk: Function,
    on_events: Function,
) -> Result<(), JsValue> {
    let digest = Digest::parse(compressed_digest).map_err(js_display)?;
    let mut source = ChunkSource { next_chunk };

    let result: Result<(), WebError> = async {
        match browser_encoding(media_type)? {
            Encoding::Tar => {
                drive_layer_stream(
                    Decoder::tar(),
                    None,
                    digest,
                    compressed_size,
                    diff_id.as_deref(),
                    &mut source,
                    &on_events,
                )
                .await
            }
            Encoding::Gzip => {
                let mut history = vec![0; gzip::HISTORY_SIZE];
                let decoder = Decoder::gzip(GzipBuffers {
                    history: &mut history,
                })
                .map_err(display_web)?;
                drive_layer_stream(
                    decoder,
                    None,
                    digest,
                    compressed_size,
                    diff_id.as_deref(),
                    &mut source,
                    &on_events,
                )
                .await
            }
            Encoding::Zstd => {
                let prefix = zstd_prefix(&mut source).await?;
                let window = zstd_window(&prefix)?;
                let mut history = vec![0; window];
                let mut block = vec![0; MAX_BLOCK_SIZE];
                let mut literals = vec![0; MAX_BLOCK_SIZE];
                let decoder = Decoder::zstd(ZstdBuffers {
                    history: &mut history,
                    block: &mut block,
                    literals: &mut literals,
                });
                drive_layer_stream(
                    decoder,
                    Some(prefix),
                    digest,
                    compressed_size,
                    diff_id.as_deref(),
                    &mut source,
                    &on_events,
                )
                .await
            }
        }
    }
    .await;
    result.map_err(js_display)
}

struct ChunkSource {
    next_chunk: Function,
}

impl ChunkSource {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, WebError> {
        loop {
            let promise = self
                .next_chunk
                .call0(&JsValue::NULL)
                .map(|value| Promise::resolve(&value))
                .map_err(js_web_error)?;
            let value = JsFuture::from(promise).await.map_err(js_web_error)?;
            if value.is_null() || value.is_undefined() {
                return Ok(None);
            }
            let bytes = Uint8Array::new(&value).to_vec();
            if !bytes.is_empty() {
                return Ok(Some(bytes));
            }
        }
    }
}

async fn zstd_prefix(source: &mut ChunkSource) -> Result<Vec<u8>, WebError> {
    let mut prefix = Vec::new();
    loop {
        match zstd::inspect_frame(&prefix).map_err(display_web)? {
            HeaderStatus::Complete { .. } => return Ok(prefix),
            HeaderStatus::NeedMore { minimum } => {
                while prefix.len() < minimum {
                    let chunk = source
                        .next()
                        .await?
                        .ok_or_else(|| WebError("truncated Zstandard frame header".to_owned()))?;
                    prefix.extend_from_slice(&chunk);
                }
            }
        }
    }
}

fn zstd_window(bytes: &[u8]) -> Result<usize, WebError> {
    let window = match zstd::inspect_frame(bytes).map_err(display_web)? {
        HeaderStatus::Complete {
            header: StreamHeader::Zstandard(header),
            ..
        } => usize::try_from(header.window_size)
            .map_err(|_| WebError("Zstandard window does not fit this browser".to_owned()))?,
        HeaderStatus::Complete {
            header: StreamHeader::Skippable { .. },
            ..
        } => {
            return Err(WebError(
                "layer starts with a skippable Zstandard frame".to_owned(),
            ))
        }
        HeaderStatus::NeedMore { .. } => {
            return Err(WebError("truncated Zstandard frame header".to_owned()))
        }
    };
    if window > MAX_ZSTD_WINDOW {
        return Err(WebError(
            "Zstandard window exceeds the 256 MiB browser limit".to_owned(),
        ));
    }
    Ok(window)
}

async fn drive_layer_stream(
    decoder: Decoder<'_>,
    prefix: Option<Vec<u8>>,
    digest: Digest,
    compressed_size: u64,
    diff_id: Option<&str>,
    source: &mut ChunkSource,
    on_events: &Function,
) -> Result<(), WebError> {
    let verified = verified_decoder(decoder, digest, compressed_size, diff_id)?;
    let mut path = vec![0; 4096];
    let mut link = vec![0; 4096];
    let mut pax = vec![0; 64 * 1024];
    let archive = Archive::new(ArchiveBuffers {
        path: &mut path,
        link: &mut link,
        pax: &mut pax,
    });
    let mut applier = LayerApplier::new(verified, archive);
    let mut sink = EventSink::default();

    if let Some(prefix) = prefix {
        push_layer_chunk(&mut applier, &mut sink, &prefix, on_events)?;
    }
    loop {
        let chunk = match source.next().await {
            Ok(chunk) => chunk,
            Err(error) => {
                sink.abort_layer();
                return Err(error);
            }
        };
        let Some(chunk) = chunk else { break };
        push_layer_chunk(&mut applier, &mut sink, &chunk, on_events)?;
    }
    applier.finish(&mut sink).map_err(display_web)?;
    flush_events(&mut sink, on_events)
}

fn push_layer_chunk(
    applier: &mut LayerApplier<'_, '_>,
    sink: &mut EventSink,
    chunk: &[u8],
    on_events: &Function,
) -> Result<(), WebError> {
    applier.push(chunk, sink).map_err(display_web)?;
    flush_events(sink, on_events)
}

fn flush_events(sink: &mut EventSink, on_events: &Function) -> Result<(), WebError> {
    if sink.events.is_empty() {
        return Ok(());
    }
    let events = std::mem::take(&mut sink.events);
    let events = serde_wasm_bindgen::to_value(&events).map_err(display_web)?;
    on_events
        .call1(&JsValue::NULL, &events)
        .map_err(js_web_error)?;
    Ok(())
}

#[wasm_bindgen]
pub fn extract_file(
    bytes: &[u8],
    media_type: &str,
    compressed_digest: &str,
    compressed_size: u64,
    diff_id: Option<String>,
    path: &str,
) -> Result<Vec<u8>, JsValue> {
    let digest = Digest::parse(compressed_digest).map_err(js_display)?;
    with_decoder(bytes, media_type, |decoder| {
        let verified = verified_decoder(decoder, digest, compressed_size, diff_id.as_deref())?;
        let mut extractor = VerifiedEntryExtractor::new(verified, path.as_bytes());
        let mut output = Vec::new();
        extractor
            .push(bytes, |chunk| {
                append_bounded(&mut output, chunk, MAX_EXTRACTED_FILE)
            })
            .map_err(display_web)?;
        extractor
            .finish(|chunk| append_bounded(&mut output, chunk, MAX_EXTRACTED_FILE))
            .map_err(display_web)?;
        Ok(output)
    })
    .map_err(js_display)
}

#[wasm_bindgen]
pub fn decode_layer(
    bytes: &[u8],
    media_type: &str,
    compressed_digest: &str,
    compressed_size: u64,
    diff_id: Option<String>,
) -> Result<Vec<u8>, JsValue> {
    let digest = Digest::parse(compressed_digest).map_err(js_display)?;
    with_decoder(bytes, media_type, |decoder| {
        let mut verified = verified_decoder(decoder, digest, compressed_size, diff_id.as_deref())?;
        let mut output = Vec::new();
        verified
            .push(bytes, |chunk| {
                append_bounded(&mut output, chunk, MAX_ARCHIVE)
            })
            .map_err(display_web)?;
        verified
            .finish(|chunk| append_bounded(&mut output, chunk, MAX_ARCHIVE))
            .map_err(display_web)?;
        Ok(output)
    })
    .map_err(js_display)
}

fn verified_decoder<'a>(
    decoder: Decoder<'a>,
    digest: Digest,
    compressed_size: u64,
    diff_id: Option<&str>,
) -> Result<VerifiedDecoder<'a>, WebError> {
    match diff_id {
        Some(diff_id) => Ok(VerifiedDecoder::new(
            decoder,
            digest,
            compressed_size,
            Digest::parse(diff_id).map_err(display_web)?,
        )),
        None => Ok(VerifiedDecoder::compressed_only(
            decoder,
            digest,
            compressed_size,
        )),
    }
}

#[wasm_bindgen]
pub fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::from("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[wasm_bindgen]
pub fn build_tar(entries: Array) -> Result<Vec<u8>, JsValue> {
    let mut output = Vec::new();
    let mut writer = TarWriter::new();
    for entry in entries.iter() {
        let path = Reflect::get(&entry, &JsValue::from_str("path"))?
            .as_string()
            .ok_or_else(|| js_error("tar entry path must be a string"))?;
        let value = Reflect::get(&entry, &JsValue::from_str("bytes"))?;
        let bytes = Uint8Array::new(&value).to_vec();
        let projected = output
            .len()
            .checked_add(bytes.len())
            .and_then(|size| size.checked_add(1536))
            .ok_or_else(|| js_error("archive size overflow"))?;
        if projected > MAX_ARCHIVE {
            return Err(js_error("archive exceeds the 256 MiB browser limit"));
        }
        writer
            .begin_file(path.as_bytes(), bytes.len() as u64, 0o644, |chunk| {
                output.extend_from_slice(chunk);
                Ok::<_, WebError>(())
            })
            .map_err(js_display)?;
        writer
            .write_file_data(&bytes, |chunk| {
                output.extend_from_slice(chunk);
                Ok::<_, WebError>(())
            })
            .map_err(js_display)?;
        writer
            .end_file(|chunk| {
                output.extend_from_slice(chunk);
                Ok::<_, WebError>(())
            })
            .map_err(js_display)?;
    }
    writer
        .finish(|chunk| {
            output.extend_from_slice(chunk);
            Ok::<_, WebError>(())
        })
        .map_err(js_display)?;
    Ok(output)
}

fn with_decoder<T>(
    bytes: &[u8],
    media_type: &str,
    run: impl FnOnce(Decoder<'_>) -> Result<T, WebError>,
) -> Result<T, WebError> {
    match browser_encoding(media_type)? {
        Encoding::Tar => run(Decoder::tar()),
        Encoding::Gzip => {
            let mut history = vec![0; gzip::HISTORY_SIZE];
            let decoder = Decoder::gzip(GzipBuffers {
                history: &mut history,
            })
            .map_err(display_web)?;
            run(decoder)
        }
        Encoding::Zstd => {
            let window = match zstd::inspect_frame(bytes).map_err(display_web)? {
                HeaderStatus::Complete {
                    header: StreamHeader::Zstandard(header),
                    ..
                } => usize::try_from(header.window_size).map_err(|_| {
                    WebError("Zstandard window does not fit this browser".to_owned())
                })?,
                HeaderStatus::Complete {
                    header: StreamHeader::Skippable { .. },
                    ..
                } => {
                    return Err(WebError(
                        "layer starts with a skippable Zstandard frame".to_owned(),
                    ))
                }
                HeaderStatus::NeedMore { .. } => {
                    return Err(WebError("truncated Zstandard frame header".to_owned()))
                }
            };
            if window > MAX_ZSTD_WINDOW {
                return Err(WebError(
                    "Zstandard window exceeds the 256 MiB browser limit".to_owned(),
                ));
            }
            let mut history = vec![0; window];
            let mut block = vec![0; MAX_BLOCK_SIZE];
            let mut literals = vec![0; MAX_BLOCK_SIZE];
            run(Decoder::zstd(ZstdBuffers {
                history: &mut history,
                block: &mut block,
                literals: &mut literals,
            }))
        }
    }
}

fn browser_encoding(media_type: &str) -> Result<Encoding, WebError> {
    if let Ok(encoding) = encoding(media_type) {
        return Ok(encoding);
    }
    if media_type.ends_with(".tar+gzip") || media_type.ends_with(".tar.gzip") {
        return Ok(Encoding::Gzip);
    }
    if media_type.ends_with(".tar+zstd") || media_type.ends_with(".tar.zstd") {
        return Ok(Encoding::Zstd);
    }
    if media_type.ends_with(".tar") {
        return Ok(Encoding::Tar);
    }
    Err(WebError(format!(
        "unsupported OCI layer media type: {media_type}"
    )))
}

fn descriptor_view(descriptor: Descriptor<'_>) -> Result<DescriptorView, JsValue> {
    let platform = descriptor
        .platform()
        .map_err(js_display)?
        .map(|platform| -> Result<PlatformView, JsValue> {
            Ok(PlatformView {
                os: owned_json(platform.os())?,
                architecture: owned_json(platform.architecture())?,
                variant: optional_json(platform.variant().map_err(js_display)?)?,
                os_version: optional_json(platform.os_version().map_err(js_display)?)?,
            })
        })
        .transpose()?;
    Ok(DescriptorView {
        media_type: owned_json(descriptor.media_type())?,
        digest: descriptor.digest().to_string(),
        size: descriptor.size(),
        artifact_type: optional_json(descriptor.artifact_type().map_err(js_display)?)?,
        platform,
        annotations: owned_annotations(descriptor.annotations().map_err(js_display)?)?,
    })
}

fn owned_annotations<'a>(
    annotations: impl Iterator<
        Item = Result<(JsonString<'a>, JsonString<'a>), oci_zero::metadata::MetadataError>,
    >,
) -> Result<Vec<(String, String)>, JsValue> {
    annotations
        .map(|annotation| {
            let (key, value) = annotation.map_err(js_display)?;
            Ok((owned_json(key)?, owned_json(value)?))
        })
        .collect()
}

fn optional_json(value: Option<JsonString<'_>>) -> Result<Option<String>, JsValue> {
    value.map(owned_json).transpose()
}

fn owned_json(value: JsonString<'_>) -> Result<String, JsValue> {
    if let Some(value) = value.as_str() {
        return Ok(value.to_owned());
    }
    let mut buffer = vec![0; value.encoded().len()];
    Ok(value
        .decode_into(&mut buffer)
        .map_err(js_display)?
        .to_owned())
}

fn entry_kind(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Regular => "regular",
        EntryKind::HardLink => "hard_link",
        EntryKind::SymbolicLink => "symbolic_link",
        EntryKind::CharacterDevice => "character_device",
        EntryKind::BlockDevice => "block_device",
        EntryKind::Directory => "directory",
        EntryKind::Fifo => "fifo",
        EntryKind::Contiguous => "contiguous",
        EntryKind::Other(_) => "other",
    }
}

fn path_text(path: &[u8]) -> String {
    match String::from_utf8_lossy(path) {
        Cow::Borrowed(path) => path.to_owned(),
        Cow::Owned(path) => path,
    }
}

fn append_bounded(output: &mut Vec<u8>, bytes: &[u8], limit: usize) -> Result<(), WebError> {
    if output.len().saturating_add(bytes.len()) > limit {
        return Err(WebError(
            "browser output exceeds its configured limit".to_owned(),
        ));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(js_display)
}

fn js_error(message: &str) -> JsValue {
    JsValue::from_str(message)
}

fn js_display(error: impl std::fmt::Display) -> JsValue {
    js_error(&error.to_string())
}

fn js_web_error(error: JsValue) -> WebError {
    WebError(
        error
            .as_string()
            .unwrap_or_else(|| format!("JavaScript stream error: {error:?}")),
    )
}

fn display_web(error: impl std::fmt::Display) -> WebError {
    WebError(error.to_string())
}

#[derive(Debug)]
struct WebError(String);

impl std::fmt::Display for WebError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
