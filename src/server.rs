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

use crate::digest::Digest;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferTooSmall;

impl fmt::Display for BufferTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("registry listing buffer too small")
    }
}

/// Serve GET/HEAD and OPTIONS from a read-only registry. Pagination accepts
/// positive `n` and a percent-encoded `last` cursor (maximum 256 decoded bytes).
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
        let mut output = Output {
            bytes: scratch,
            len: 0,
        };
        if let Some(repo) = repository {
            output.write_str("{\"name\":").map_err(|_| BufferTooSmall)?;
            json_string(&mut output, repo).map_err(|_| BufferTooSmall)?;
            output
                .write_str(",\"tags\":[")
                .map_err(|_| BufferTooSmall)?;
        } else {
            output
                .write_str("{\"repositories\":[")
                .map_err(|_| BufferTooSmall)?;
        }
        let mut previous = last;
        let mut count = 0;
        let mut emitted = None;
        let mut next = None;
        loop {
            // Selection avoids allocation and imposes no sorting requirement on
            // small embedded stores. Large stores can implement indexed access.
            let mut smallest = None;
            let mut index = 0;
            while let Some(name) = name_at(index) {
                if name > previous && smallest.map_or(true, |best| name < best) {
                    smallest = Some(name);
                }
                index += 1;
            }
            let Some(name) = smallest else { break };
            if count == n {
                next = Some(NextLink {
                    path,
                    last: emitted.unwrap(),
                    n,
                });
                break;
            }
            if count != 0 {
                output.write_str(",").map_err(|_| BufferTooSmall)?;
            }
            json_string(&mut output, name).map_err(|_| BufferTooSmall)?;
            previous = name;
            emitted = Some(name);
            count += 1;
        }
        output.write_str("]}").map_err(|_| BufferTooSmall)?;
        let length = output.len;
        let mut result = response(200, &scratch[..length], "application/json", None, head);
        result.next = next;
        return Ok(result);
    }
    if let Some((repo, reference)) = rest.rsplit_once("/manifests/") {
        if !store.contains_repository(repo) {
            return Ok(error(404, "NAME_UNKNOWN", head));
        }
        return Ok(match store.manifest(repo, reference) {
            Some(content) => response(
                200,
                content.bytes,
                content.media_type,
                Some(content.digest),
                head,
            ),
            None => error(404, "MANIFEST_UNKNOWN", head),
        });
    }
    if let Some((repo, encoded)) = rest.rsplit_once("/blobs/") {
        if !store.contains_repository(repo) {
            return Ok(error(404, "NAME_UNKNOWN", head));
        }
        let Ok(digest) = Digest::parse(encoded) else {
            return Ok(error(400, "DIGEST_INVALID", head));
        };
        return Ok(match store.blob(repo, digest) {
            Some(content) => response(
                200,
                content.bytes,
                "application/octet-stream",
                Some(content.digest),
                head,
            ),
            None => error(404, "BLOB_UNKNOWN", head),
        });
    }
    Ok(error(404, "NAME_UNKNOWN", head))
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
    let mut n = usize::MAX;
    let mut last = "";
    let mut seen_n = false;
    let mut seen_last = false;
    for parameter in query.split('&').filter(|item| !item.is_empty()) {
        let (key, value) = parameter.split_once('=').unwrap_or((parameter, ""));
        match key {
            "n" => {
                if seen_n || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                seen_n = true;
                n = value.parse().ok()?;
                if n == 0 {
                    return None;
                }
            }
            "last" => {
                if seen_last {
                    return None;
                }
                seen_last = true;
                last = value;
            }
            _ => {}
        }
    }
    let mut bytes = last.bytes();
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
    Some((n, core::str::from_utf8(&cursor[..len]).ok()?))
}

struct Output<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl Write for Output<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn json_string(output: &mut impl Write, value: &str) -> fmt::Result {
    output.write_char('"')?;
    for ch in value.chars() {
        match ch {
            '"' => output.write_str("\\\"")?,
            '\\' => output.write_str("\\\\")?,
            '\u{0}'..='\u{1f}' => write!(output, "\\u{:04x}", ch as u32)?,
            _ => output.write_char(ch)?,
        }
    }
    output.write_char('"')
}
