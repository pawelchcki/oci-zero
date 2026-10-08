//! A read-only OCI Distribution router, independent of HTTP and storage runtimes.
//!
//! Supply a [`Store`] (borrowed memory, flash, or a memfs), and pass an HTTP method
//! and origin-form request target to [`serve`]. Only listings use the caller's
//! scratch buffer; manifests and blobs are borrowed directly from the store.
//! Transports must send the response metadata, including HEAD's content length,
//! and may add CORS, authentication, caching, or TLS outside this module.
//! Uploads, deletion, referrers, ranges, and Accept negotiation are not supported.

use core::fmt::{self, Write};

use sha2::{Digest as _, Sha256};

use crate::{buffer::BufferWriter, digest::Digest};

/// Immutable bytes and their OCI media type and content digest.
#[derive(Clone, Copy, Debug)]
pub struct Content<'a> {
    pub bytes: &'a [u8],
    pub media_type: &'a str,
    pub digest: Digest,
}

impl<'a> Content<'a> {
    /// Hash once when constructing the store, rather than on each request.
    pub fn new(bytes: &'a [u8], media_type: &'a str) -> Self {
        Self {
            bytes,
            media_type,
            digest: Digest::from_bytes(Sha256::digest(bytes).into()),
        }
    }
}

/// Storage contract for a read-only registry.
///
/// Names must follow OCI repository/tag grammar and be unique. Indexed lists
/// terminate at the first `None`; their order need not be sorted. Content digests
/// must describe the exact bytes returned. Blob lookup is repository scoped.
pub trait Store {
    fn repository(&self, index: usize) -> Option<&str>;
    fn tag(&self, repository: &str, index: usize) -> Option<&str>;
    fn contains_repository(&self, repository: &str) -> bool;
    fn manifest(&self, repository: &str, reference: &str) -> Option<Content<'_>>;
    fn blob(&self, repository: &str, digest: Digest) -> Option<Content<'_>>;
}

/// A tag alias for a manifest in [`Repository::manifests`].
#[derive(Clone, Copy, Debug)]
pub struct Tag<'a> {
    pub name: &'a str,
    pub digest: Digest,
}

/// A borrowed repository, including untagged manifests (e.g. index children).
#[derive(Clone, Copy, Debug)]
pub struct Repository<'a> {
    pub name: &'a str,
    pub tags: &'a [Tag<'a>],
    pub manifests: &'a [Content<'a>],
    pub blobs: &'a [Content<'a>],
}

/// An allocation-free store of borrowed slices. Fields may point to ROM,
/// `include_bytes!` assets, or buffers owned by the caller.
#[derive(Clone, Copy, Debug)]
pub struct MemoryStore<'a>(pub &'a [Repository<'a>]);

impl MemoryStore<'_> {
    fn find(&self, name: &str) -> Option<&Repository<'_>> {
        self.0.iter().find(|repo| repo.name == name)
    }
}

impl Store for MemoryStore<'_> {
    fn repository(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(|repo| repo.name)
    }

    fn tag(&self, repository: &str, index: usize) -> Option<&str> {
        self.find(repository)?.tags.get(index).map(|tag| tag.name)
    }

    fn contains_repository(&self, repository: &str) -> bool {
        self.find(repository).is_some()
    }

    fn manifest(&self, repository: &str, reference: &str) -> Option<Content<'_>> {
        let repo = self.find(repository)?;
        let digest = if let Ok(digest) = Digest::parse(reference) {
            digest
        } else {
            repo.tags.iter().find(|tag| tag.name == reference)?.digest
        };
        repo.manifests
            .iter()
            .find(|item| item.digest == digest)
            .copied()
    }

    fn blob(&self, repository: &str, digest: Digest) -> Option<Content<'_>> {
        self.find(repository)?
            .blobs
            .iter()
            .find(|item| item.digest == digest)
            .copied()
    }
}

/// Metadata and body to pass to an HTTP transport.
#[derive(Debug)]
pub struct Response<'a> {
    pub status: u16,
    pub media_type: &'a str,
    pub body: &'a [u8],
    /// GET representation length, even when HEAD suppresses the body.
    pub content_length: usize,
    pub digest: Option<Digest>,
    pub next: Option<NextLink<'a>>,
}

/// Format as an HTTP `Link` header. The cursor is percent encoded.
#[derive(Clone, Copy, Debug)]
pub struct NextLink<'a> {
    pub path: &'a str,
    pub last: &'a str,
    pub n: usize,
}

impl fmt::Display for NextLink<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{}?n={}&last=", self.path, self.n)?;
        for byte in self.last.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                write!(f, "{}", byte as char)?;
            } else {
                write!(f, "%{byte:02X}")?;
            }
        }
        f.write_str(">; rel=\"next\"")
    }
}

/// The scratch buffer cannot hold the generated listing. No partial response
/// is returned; the transport can retry with more space or return HTTP 500.
#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display)]
#[display("registry listing buffer too small")]
pub struct BufferTooSmall;

impl From<fmt::Error> for BufferTooSmall {
    fn from(_: fmt::Error) -> Self {
        Self
    }
}

/// Serve GET/HEAD and OPTIONS from a read-only registry. Pagination accepts
/// nonnegative `n` and a percent-encoded `last` cursor (maximum 256 decoded bytes).
/// `n=0` returns an empty page without a continuation link.
/// Unknown query parameters are ignored. Malformed known parameters return 400.
/// Listings are lexically sorted even if the store's names are unsorted.
pub fn serve<'a>(
    store: &'a impl Store,
    method: &str,
    target: &'a str,
    scratch: &'a mut [u8],
) -> Result<Response<'a>, BufferTooSmall> {
    let head = method == "HEAD";
    if method == "OPTIONS" {
        return Ok(response(204, b"", "application/json", None, false));
    }
    if method != "GET" && !head {
        return Ok(error(405, "UNSUPPORTED", head));
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path == "/v2/" || path == "/v2" {
        return Ok(response(200, b"{}", "application/json", None, head));
    }
    let Some(rest) = path.strip_prefix("/v2/") else {
        return Ok(error(404, "NAME_UNKNOWN", head));
    };
    let listing = if rest == "_catalog" {
        Some(None)
    } else {
        rest.strip_suffix("/tags/list").map(Some)
    };
    if let Some(repository) = listing {
        return serve_listing(store, repository, path, query, scratch, head);
    }
    let Some((prefix, reference)) = rest.rsplit_once('/') else {
        return Ok(error(404, "NAME_UNKNOWN", head));
    };
    let Some((repo, operation)) = prefix.rsplit_once('/') else {
        return Ok(error(404, "NAME_UNKNOWN", head));
    };
    if !store.contains_repository(repo) {
        return Ok(error(404, "NAME_UNKNOWN", head));
    }
    let (content, missing) = match operation {
        "manifests" => (store.manifest(repo, reference), "MANIFEST_UNKNOWN"),
        "blobs" => {
            let Ok(digest) = Digest::parse(reference) else {
                return Ok(error(400, "DIGEST_INVALID", head));
            };
            (
                store.blob(repo, digest).map(|content| Content {
                    media_type: "application/octet-stream",
                    ..content
                }),
                "BLOB_UNKNOWN",
            )
        }
        _ => return Ok(error(404, "NAME_UNKNOWN", head)),
    };
    Ok(match content {
        Some(content) => response(
            200,
            content.bytes,
            content.media_type,
            Some(content.digest),
            head,
        ),
        None => error(404, missing, head),
    })
}

fn serve_listing<'a>(
    store: &'a impl Store,
    repository: Option<&'a str>,
    path: &'a str,
    query: &str,
    scratch: &'a mut [u8],
    head: bool,
) -> Result<Response<'a>, BufferTooSmall> {
    if repository.is_some_and(|repo| !store.contains_repository(repo)) {
        return Ok(error(404, "NAME_UNKNOWN", head));
    }
    let mut cursor = [0u8; 256];
    let (n, last) = match pagination(query, &mut cursor) {
        Some(value) => value,
        None => return Ok(error(400, "UNSUPPORTED", head)),
    };
    let name_at = |index| match repository {
        Some(repo) => store.tag(repo, index),
        None => store.repository(index),
    };
    let mut output = BufferWriter::new(scratch);
    if let Some(repo) = repository {
        write!(output, r#"{{"name":"{repo}","tags":["#)?;
    } else {
        output.write_str(r#"{"repositories":["#)?;
    }
    let mut previous = last;
    let mut count = 0;
    let mut emitted = None;
    let mut next = None;
    loop {
        if n == 0 {
            break;
        }
        // Selection avoids allocation and imposes no sorting requirement on
        // small embedded stores. Large stores can implement indexed access.
        let Some(name) = (0..)
            .map_while(&name_at)
            .filter(|name| *name > previous)
            .min()
        else {
            break;
        };
        if count == n {
            next = Some(NextLink {
                path,
                last: emitted.unwrap(),
                n,
            });
            break;
        }
        if count != 0 {
            output.write_str(",")?;
        }
        // Store names follow OCI grammar: ASCII with no JSON escapes.
        write!(output, "\"{name}\"")?;
        previous = name;
        emitted = Some(name);
        count += 1;
    }
    output.write_str("]}")?;
    let length = output.len();
    let mut result = response(200, &scratch[..length], "application/json", None, head);
    result.next = next;
    Ok(result)
}

fn response<'a>(
    status: u16,
    bytes: &'a [u8],
    media_type: &'a str,
    digest: Option<Digest>,
    head: bool,
) -> Response<'a> {
    Response {
        status,
        media_type,
        body: if head { b"" } else { bytes },
        content_length: bytes.len(),
        digest,
        next: None,
    }
}

fn error(status: u16, code: &str, head: bool) -> Response<'static> {
    let bytes: &[u8] = match code {
        "MANIFEST_UNKNOWN" => {
            br#"{"errors":[{"code":"MANIFEST_UNKNOWN","message":"manifest unknown"}]}"#
        }
        "BLOB_UNKNOWN" => br#"{"errors":[{"code":"BLOB_UNKNOWN","message":"blob unknown"}]}"#,
        "DIGEST_INVALID" => br#"{"errors":[{"code":"DIGEST_INVALID","message":"invalid digest"}]}"#,
        "UNSUPPORTED" => br#"{"errors":[{"code":"UNSUPPORTED","message":"unsupported request"}]}"#,
        _ => br#"{"errors":[{"code":"NAME_UNKNOWN","message":"repository unknown"}]}"#,
    };
    response(status, bytes, "application/json", None, head)
}

fn pagination<'a>(query: &str, cursor: &'a mut [u8]) -> Option<(usize, &'a str)> {
    let mut n = None;
    let mut last = None;
    for parameter in query.split('&').filter(|item| !item.is_empty()) {
        let (key, value) = parameter.split_once('=').unwrap_or((parameter, ""));
        match key {
            "n" => {
                if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                if n.replace(value.parse().ok()?).is_some() {
                    return None;
                }
            }
            "last" if last.replace(value).is_some() => return None,
            _ => {}
        }
    }
    let mut bytes = last.unwrap_or("").bytes();
    let mut len = 0;
    while let Some(byte) = bytes.next() {
        let decoded = if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else if byte == b'+' {
            b' '
        } else {
            byte
        };
        *cursor.get_mut(len)? = decoded;
        len += 1;
    }
    Some((
        n.unwrap_or(usize::MAX),
        core::str::from_utf8(&cursor[..len]).ok()?,
    ))
}
