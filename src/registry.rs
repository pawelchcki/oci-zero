//! Transport-neutral OCI Distribution request and response policy.

use core::{fmt, str};

use crate::{
    buffer::BufferWriter,
    digest::Digest,
    json::Value,
    metadata::{JsonString, MetadataError},
    reference::{Reference, ReferenceError, Repository, Scheme, Selector},
};

pub const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.docker.distribution.manifest.v2+json"
);

pub const OCI_INDEX_ACCEPT: &str = "application/vnd.oci.image.index.v1+json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Head,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target<'a> {
    pub scheme: Scheme,
    pub authority: &'a str,
    pub path_and_query: &'a str,
}

impl<'a> Target<'a> {
    pub fn parse(url: &'a str) -> Result<Self, RegistryError> {
        let (scheme, remainder) = if let Some(value) = url.strip_prefix("https://") {
            (Scheme::Https, value)
        } else if let Some(value) = url.strip_prefix("http://") {
            (Scheme::Http, value)
        } else {
            return Err(RegistryError::InvalidUrl);
        };
        let (authority, path_and_query) = match remainder.find('/') {
            Some(index) => (&remainder[..index], &remainder[index..]),
            None => (remainder, "/"),
        };
        if authority.is_empty()
            || !authority.is_ascii()
            || authority
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'@' | b'#'))
            || !path_and_query.starts_with('/')
        {
            return Err(RegistryError::InvalidUrl);
        }
        Ok(Self {
            scheme,
            authority,
            path_and_query,
        })
    }

    pub fn same_origin(&self, other: &Self) -> bool {
        self.scheme == other.scheme && self.authority.eq_ignore_ascii_case(other.authority)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request<'a> {
    pub method: Method,
    pub target: Target<'a>,
    pub accept: &'a str,
    pub authorization: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOperation {
    Probe,
    Catalog,
    Manifest,
    Blob,
    Referrers,
    Tags,
}

pub struct RequestPlanner<'reference> {
    repository: Repository<'reference>,
    selector: Option<Selector<'reference>>,
    scheme: Scheme,
}

impl<'reference> RequestPlanner<'reference> {
    pub const fn new(reference: Reference<'reference>) -> Self {
        Self::with_scheme(reference, Scheme::Https)
    }

    pub const fn with_scheme(reference: Reference<'reference>, scheme: Scheme) -> Self {
        Self {
            repository: reference.as_repository(),
            selector: Some(reference.selector()),
            scheme,
        }
    }

    pub const fn for_repository(repository: Repository<'reference>) -> Self {
        Self::for_repository_with_scheme(repository, Scheme::Https)
    }

    pub const fn for_repository_with_scheme(
        repository: Repository<'reference>,
        scheme: Scheme,
    ) -> Self {
        Self {
            repository,
            selector: None,
            scheme,
        }
    }

    pub fn probe<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        copy_request(
            buffer,
            self.scheme,
            self.repository.registry(),
            "/v2/",
            "*/*",
        )
    }

    pub fn manifest<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        let selector = self.selector.ok_or(ReferenceError::MissingSelector)?;
        Ok(self.request(
            self.repository.manifest_path(selector, buffer)?,
            MANIFEST_ACCEPT,
        ))
    }

    pub fn head_manifest<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        let mut request = self.manifest(buffer)?;
        request.method = Method::Head;
        Ok(request)
    }

    pub fn manifest_by_digest<'buffer>(
        &'buffer self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        Ok(self.request(
            self.repository.manifest_digest_path(digest, buffer)?,
            MANIFEST_ACCEPT,
        ))
    }

    pub fn blob<'buffer>(
        &'buffer self,
        digest: Digest,
        accept: &'buffer str,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        Ok(self.request(self.repository.blob_path(digest, buffer)?, accept))
    }

    pub fn head_blob<'buffer>(
        &'buffer self,
        digest: Digest,
        accept: &'buffer str,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        let mut request = self.blob(digest, accept, buffer)?;
        request.method = Method::Head;
        Ok(request)
    }

    pub fn referrers<'buffer>(
        &'buffer self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        Ok(self.request(
            self.repository.referrers_path(digest, buffer)?,
            OCI_INDEX_ACCEPT,
        ))
    }

    pub fn tags<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        Ok(self.request(self.repository.tags_path(buffer)?, "application/json"))
    }

    pub fn tags_page<'buffer>(
        &'buffer self,
        page_size: u16,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        if page_size == 0 {
            return Err(RegistryError::InvalidPageSize);
        }
        Ok(self.request(
            self.repository.tags_page_path(page_size, buffer)?,
            "application/json",
        ))
    }

    fn request<'buffer>(
        &'buffer self,
        path: &'buffer str,
        accept: &'buffer str,
    ) -> Request<'buffer> {
        get_request(self.scheme, self.repository.registry(), path, accept)
    }

    pub fn referrers_fallback<'buffer>(
        &'buffer self,
        digest: Digest,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        let mut digest_text = [0; 71];
        let length = write_display(&mut digest_text, digest)?;
        digest_text[6] = b'-';
        let mut path = [0; 384];
        let prefix = b"/v2/";
        let middle = b"/manifests/";
        let needed = prefix.len() + self.repository.repository().len() + middle.len() + length;
        if needed > path.len() {
            return Err(RegistryError::BufferTooSmall);
        }
        let mut position = 0;
        for part in [
            prefix.as_slice(),
            self.repository.repository().as_bytes(),
            middle.as_slice(),
            &digest_text[..length],
        ] {
            path[position..position + part.len()].copy_from_slice(part);
            position += part.len();
        }
        let path = str::from_utf8(&path[..position]).map_err(|_| RegistryError::InvalidUrl)?;
        copy_request(
            buffer,
            self.scheme,
            self.repository.registry(),
            path,
            MANIFEST_ACCEPT,
        )
    }
}

/// Plans registry-wide requests that do not require a repository name.
pub struct RegistryRequestPlanner<'authority> {
    authority: &'authority str,
    scheme: Scheme,
}

impl<'authority> RegistryRequestPlanner<'authority> {
    pub fn new(authority: &'authority str) -> Result<Self, RegistryError> {
        Self::with_scheme(authority, Scheme::Https)
    }

    pub fn with_scheme(authority: &'authority str, scheme: Scheme) -> Result<Self, RegistryError> {
        validate_authority(authority)?;
        Ok(Self { authority, scheme })
    }

    pub fn probe<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        copy_request(buffer, self.scheme, self.authority, "/v2/", "*/*")
    }

    pub fn catalog<'buffer>(
        &'buffer self,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        copy_request(
            buffer,
            self.scheme,
            self.authority,
            "/v2/_catalog",
            "application/json",
        )
    }

    pub fn catalog_page<'buffer>(
        &'buffer self,
        page_size: u16,
        buffer: &'buffer mut [u8],
    ) -> Result<Request<'buffer>, RegistryError> {
        if page_size == 0 {
            return Err(RegistryError::InvalidPageSize);
        }
        const PREFIX: &[u8] = b"/v2/_catalog?n=";
        if buffer.len() < PREFIX.len() {
            return Err(RegistryError::BufferTooSmall);
        }
        buffer[..PREFIX.len()].copy_from_slice(PREFIX);
        let digits = write_display(&mut buffer[PREFIX.len()..], page_size)?;
        request_from_path(
            buffer,
            PREFIX.len() + digits,
            self.scheme,
            self.authority,
            "application/json",
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseHead<'a> {
    pub status: u16,
    pub headers: &'a [Header<'a>],
}

impl<'a> ResponseHead<'a> {
    pub fn header(&self, name: &str) -> Option<&'a [u8]> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value)
    }

    pub fn classify(&self, allow_not_found: bool) -> Result<ResponseAction<'a>, RegistryError> {
        match self.status {
            200..=299 => Ok(ResponseAction::Success),
            301 | 302 | 303 | 307 | 308 => {
                let location = self
                    .header("location")
                    .ok_or(RegistryError::MissingHeader("Location"))?;
                Ok(ResponseAction::Redirect(
                    str::from_utf8(location).map_err(|_| RegistryError::InvalidHeader)?,
                ))
            }
            401 => {
                let challenge = self
                    .header("www-authenticate")
                    .ok_or(RegistryError::MissingHeader("WWW-Authenticate"))?;
                Ok(ResponseAction::Authenticate(AuthChallenge::parse(
                    str::from_utf8(challenge).map_err(|_| RegistryError::InvalidHeader)?,
                )?))
            }
            404 if allow_not_found => Ok(ResponseAction::NotFound),
            429 | 500..=599 => Ok(ResponseAction::Retry(RetryAdvice {
                status: self.status,
                after: self
                    .header("retry-after")
                    .map(parse_retry_after)
                    .transpose()?,
            })),
            status => Err(RegistryError::HttpStatus(status)),
        }
    }

    pub fn next_link(&self) -> Result<Option<&'a str>, RegistryError> {
        let Some(value) = self.header("link") else {
            return Ok(None);
        };
        let value = str::from_utf8(value).map_err(|_| RegistryError::InvalidHeader)?;
        for link in value.split(',') {
            let link = link.trim();
            let Some(end) = link.find('>') else {
                continue;
            };
            if !link.starts_with('<') {
                continue;
            }
            if link[end + 1..].split(';').any(|parameter| {
                parameter.trim().eq_ignore_ascii_case("rel=\"next\"")
                    || parameter.trim().eq_ignore_ascii_case("rel=next")
            }) {
                return Ok(Some(&link[1..end]));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponseAction<'a> {
    Success,
    Redirect(&'a str),
    Authenticate(AuthChallenge<'a>),
    NotFound,
    Retry(RetryAdvice<'a>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryAdvice<'a> {
    pub status: u16,
    pub after: Option<RetryAfter<'a>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryAfter<'a> {
    Seconds(u64),
    HttpDate(&'a str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthChallenge<'a> {
    Bearer(BearerChallenge<'a>),
    Basic { realm: Option<&'a str> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BearerChallenge<'a> {
    pub realm: &'a str,
    pub service: Option<&'a str>,
    pub scope: Option<&'a str>,
}

impl<'a> AuthChallenge<'a> {
    pub fn parse(value: &'a str) -> Result<Self, RegistryError> {
        let (scheme, parameters) = value
            .split_once(' ')
            .map(|(scheme, parameters)| (scheme, parameters.trim()))
            .unwrap_or((value, ""));
        if scheme.eq_ignore_ascii_case("bearer") {
            let mut realm = None;
            let mut service = None;
            let mut scope = None;
            let mut remaining = parameters;
            while !remaining.trim_start().is_empty() {
                remaining = remaining.trim_start();
                let equals = remaining.find('=').ok_or(RegistryError::InvalidChallenge)?;
                let name = remaining[..equals].trim();
                remaining = &remaining[equals + 1..];
                let (parameter, rest) = quoted_parameter(remaining)?;
                match name {
                    "realm" => realm = unique(realm, parameter)?,
                    "service" => service = unique(service, parameter)?,
                    "scope" => scope = unique(scope, parameter)?,
                    _ => {}
                }
                remaining = rest.trim_start();
                if let Some(rest) = remaining.strip_prefix(',') {
                    remaining = rest;
                } else if !remaining.is_empty() {
                    return Err(RegistryError::InvalidChallenge);
                }
            }
            Ok(Self::Bearer(BearerChallenge {
                realm: realm.ok_or(RegistryError::InvalidChallenge)?,
                service,
                scope,
            }))
        } else if scheme.eq_ignore_ascii_case("basic") {
            let realm = parameters
                .strip_prefix("realm=")
                .map(quoted_parameter)
                .transpose()?
                .map(|value| value.0);
            Ok(Self::Basic { realm })
        } else {
            Err(RegistryError::UnsupportedAuthentication)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenResponse<'a> {
    pub token: JsonString<'a>,
    pub expires_in: Option<u64>,
}

impl<'a> TokenResponse<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RegistryError> {
        let object = Value::parse_document(bytes)?.object()?;
        let token = match (object.get("token")?, object.get("access_token")?) {
            (Some(token), _) | (None, Some(token)) => token.string()?,
            (None, None) => return Err(RegistryError::InvalidTokenResponse),
        };
        let expires_in = object.get("expires_in")?.map(Value::u64).transpose()?;
        Ok(Self { token, expires_in })
    }

    /// Writes an HTTP `Bearer` authorization value into caller-owned storage.
    pub fn bearer_authorization(self, buffer: &mut [u8]) -> Result<&str, RegistryError> {
        const PREFIX: &[u8] = b"Bearer ";
        if buffer.len() < PREFIX.len() {
            return Err(RegistryError::BufferTooSmall);
        }
        buffer[..PREFIX.len()].copy_from_slice(PREFIX);
        let token_length = self
            .token
            .decode_into(&mut buffer[PREFIX.len()..])
            .map_err(MetadataError::from)?
            .len();
        let length = PREFIX.len() + token_length;
        let token = &buffer[PREFIX.len()..length];
        if token.is_empty() || !token.iter().all(|byte| (b'!'..=b'~').contains(byte)) {
            return Err(RegistryError::InvalidTokenResponse);
        }
        str::from_utf8(&buffer[..length]).map_err(|_| RegistryError::InvalidTokenResponse)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Credentials<'a> {
    pub username: &'a str,
    pub password: &'a str,
}

/// Writes an HTTP Basic authorization value into caller-owned storage.
pub fn basic_authorization<'buffer>(
    credentials: Credentials<'_>,
    buffer: &'buffer mut [u8],
) -> Result<&'buffer str, RegistryError> {
    const PREFIX: &[u8] = b"Basic ";
    if credentials.username.contains(':')
        || credentials
            .username
            .bytes()
            .chain(credentials.password.bytes())
            .any(|byte| matches!(byte, b'\r' | b'\n'))
    {
        return Err(RegistryError::InvalidCredentials);
    }
    let mut input = credentials
        .username
        .bytes()
        .chain(core::iter::once(b':'))
        .chain(credentials.password.bytes());
    let mut output = PREFIX.len();
    buffer
        .get_mut(..PREFIX.len())
        .ok_or(RegistryError::BufferTooSmall)?
        .copy_from_slice(PREFIX);
    while let Some(first) = input.next() {
        let second = input.next();
        let third = input.next();
        let bytes = [first, second.unwrap_or(0), third.unwrap_or(0)];
        let length = 1 + usize::from(second.is_some()) + usize::from(third.is_some());
        let end = output.checked_add(4).ok_or(RegistryError::BufferTooSmall)?;
        let destination = buffer
            .get_mut(output..end)
            .ok_or(RegistryError::BufferTooSmall)?;
        use base64ct::{Base64, Encoding};
        Base64::encode(&bytes[..length], destination).map_err(|_| RegistryError::BufferTooSmall)?;
        output = end;
    }
    str::from_utf8(&buffer[..output]).map_err(|_| RegistryError::InvalidCredentials)
}

/// Builds the anonymous OAuth token URL described by a Bearer challenge.
pub fn bearer_token_url<'buffer>(
    challenge: BearerChallenge<'_>,
    buffer: &'buffer mut [u8],
) -> Result<&'buffer str, RegistryError> {
    let mut length = 0;
    append(buffer, &mut length, challenge.realm.as_bytes())?;
    let mut separator = if challenge.realm.contains('?') {
        b'&'
    } else {
        b'?'
    };
    for (name, value) in [("service", challenge.service), ("scope", challenge.scope)] {
        let Some(value) = value else {
            continue;
        };
        append(buffer, &mut length, &[separator])?;
        append(buffer, &mut length, name.as_bytes())?;
        append(buffer, &mut length, b"=")?;
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                append(buffer, &mut length, &[byte])?;
            } else {
                append(
                    buffer,
                    &mut length,
                    &[b'%', HEX[(byte >> 4) as usize], HEX[(byte & 0x0f) as usize]],
                )?;
            }
        }
        separator = b'&';
    }
    str::from_utf8(&buffer[..length]).map_err(|_| RegistryError::InvalidChallenge)
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

pub trait CredentialProvider {
    type Error;

    fn credentials<'a>(
        &'a mut self,
        authority: &str,
        scope: Option<&str>,
    ) -> Result<Option<Credentials<'a>>, Self::Error>;
}

pub trait ExternalUrlPolicy {
    fn allow(&mut self, target: &Target<'_>) -> bool;
}

pub fn validate_redirect<'a>(
    current: Target<'_>,
    location: &'a str,
    allow_insecure_downgrade: bool,
) -> Result<Redirect<'a>, RegistryError> {
    let target = if location.starts_with("https://") || location.starts_with("http://") {
        Target::parse(location)?
    } else {
        return Err(RegistryError::RelativeRedirectRequiresResolution);
    };
    if current.scheme == Scheme::Https && target.scheme == Scheme::Http && !allow_insecure_downgrade
    {
        return Err(RegistryError::InsecureRedirect);
    }
    Ok(Redirect {
        strip_authorization: !current.same_origin(&target),
        target,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Redirect<'a> {
    pub target: Target<'a>,
    pub strip_authorization: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, derive_more::Display, derive_more::From)]
pub enum RegistryError {
    #[display("invalid registry reference: {_0}")]
    #[from]
    Reference(ReferenceError),
    #[display("invalid registry metadata: {_0}")]
    #[from(MetadataError, crate::json::JsonError)]
    Metadata(MetadataError),
    #[display("invalid registry URL")]
    InvalidUrl,
    #[display("invalid registry response header")]
    InvalidHeader,
    #[display("registry response lacks {_0}")]
    MissingHeader(&'static str),
    #[display("invalid authentication challenge")]
    InvalidChallenge,
    #[display("unsupported registry authentication scheme")]
    UnsupportedAuthentication,
    #[display("invalid registry token response")]
    InvalidTokenResponse,
    #[display("registry returned HTTP {_0}")]
    HttpStatus(u16),
    #[display("registry scratch buffer is too small")]
    BufferTooSmall,
    #[display("registry page size must be non-zero")]
    InvalidPageSize,
    #[display("HTTPS to HTTP redirect was rejected")]
    InsecureRedirect,
    #[display("relative redirect requires transport URL resolution")]
    RelativeRedirectRequiresResolution,
    #[display("invalid registry credentials")]
    InvalidCredentials,
}

fn request_from_path<'a>(
    buffer: &'a mut [u8],
    path_length: usize,
    scheme: Scheme,
    authority: &'a str,
    accept: &'a str,
) -> Result<Request<'a>, RegistryError> {
    let path = str::from_utf8(&buffer[..path_length]).map_err(|_| RegistryError::InvalidUrl)?;
    Ok(get_request(scheme, authority, path, accept))
}

fn get_request<'a>(
    scheme: Scheme,
    authority: &'a str,
    path: &'a str,
    accept: &'a str,
) -> Request<'a> {
    Request {
        method: Method::Get,
        target: Target {
            scheme,
            authority,
            path_and_query: path,
        },
        accept,
        authorization: None,
    }
}

fn validate_authority(authority: &str) -> Result<(), RegistryError> {
    if authority.is_empty()
        || !authority.is_ascii()
        || authority
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'@' | b'?' | b'#'))
    {
        return Err(RegistryError::InvalidUrl);
    }
    Ok(())
}

fn copy_request<'a>(
    buffer: &'a mut [u8],
    scheme: Scheme,
    authority: &'a str,
    path: &str,
    accept: &'a str,
) -> Result<Request<'a>, RegistryError> {
    let destination = buffer
        .get_mut(..path.len())
        .ok_or(RegistryError::BufferTooSmall)?;
    destination.copy_from_slice(path.as_bytes());
    request_from_path(buffer, path.len(), scheme, authority, accept)
}

fn append(buffer: &mut [u8], length: &mut usize, bytes: &[u8]) -> Result<(), RegistryError> {
    crate::buffer::append(buffer, length, bytes).map_err(|_| RegistryError::BufferTooSmall)
}

fn parse_retry_after(value: &[u8]) -> Result<RetryAfter<'_>, RegistryError> {
    let value = str::from_utf8(value).map_err(|_| RegistryError::InvalidHeader)?;
    Ok(match value.trim().parse() {
        Ok(seconds) => RetryAfter::Seconds(seconds),
        Err(_) => RetryAfter::HttpDate(value.trim()),
    })
}

fn quoted_parameter(value: &str) -> Result<(&str, &str), RegistryError> {
    let value = value.trim_start();
    if !value.starts_with('"') {
        return Err(RegistryError::InvalidChallenge);
    }
    let bytes = value.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index = index
                    .checked_add(2)
                    .ok_or(RegistryError::InvalidChallenge)?
            }
            b'"' => return Ok((&value[1..index], &value[index + 1..])),
            _ => index += 1,
        }
    }
    Err(RegistryError::InvalidChallenge)
}

fn unique<'a>(current: Option<&'a str>, value: &'a str) -> Result<Option<&'a str>, RegistryError> {
    if current.is_some() {
        Err(RegistryError::InvalidChallenge)
    } else {
        Ok(Some(value))
    }
}

fn write_display(buffer: &mut [u8], value: impl fmt::Display) -> Result<usize, RegistryError> {
    let mut writer = BufferWriter::new(buffer);
    fmt::write(&mut writer, format_args!("{value}")).map_err(|_| RegistryError::BufferTooSmall)?;
    Ok(writer.len())
}

#[cfg(test)]
mod tests {
    use super::{
        basic_authorization, bearer_token_url, validate_redirect, AuthChallenge, Credentials,
        Header, Redirect, RegistryError, RegistryRequestPlanner, RequestPlanner, ResponseAction,
        ResponseHead, RetryAfter, Target, TokenResponse,
    };
    use crate::reference::{Repository, Scheme};

    #[test]
    fn plans_paginated_catalogs_and_tags() {
        let registry = RegistryRequestPlanner::new("registry.example").unwrap();
        let mut catalog_path = [0; 64];
        assert_eq!(
            registry
                .catalog_page(100, &mut catalog_path)
                .unwrap()
                .target
                .path_and_query,
            "/v2/_catalog?n=100"
        );
        assert!(matches!(
            registry.catalog_page(0, &mut catalog_path),
            Err(RegistryError::InvalidPageSize)
        ));

        let repository = Repository::parse("oci://registry.example/team/image").unwrap();
        let planner = RequestPlanner::for_repository(repository);
        let mut tags_path = [0; 64];
        assert_eq!(
            planner
                .tags_page(50, &mut tags_path)
                .unwrap()
                .target
                .path_and_query,
            "/v2/team/image/tags/list?n=50"
        );
        assert!(matches!(
            planner.manifest(&mut tags_path),
            Err(RegistryError::Reference(
                crate::reference::ReferenceError::MissingSelector
            ))
        ));
    }

    #[test]
    fn parses_bearer_challenges() {
        let challenge = AuthChallenge::parse(
            r#"Bearer realm="https://auth.example/token",service="registry",scope="repository:a/b:pull""#,
        )
        .unwrap();
        let AuthChallenge::Bearer(challenge) = challenge else {
            panic!("expected bearer challenge");
        };
        assert_eq!(challenge.realm, "https://auth.example/token");
        assert_eq!(challenge.service, Some("registry"));
        assert_eq!(challenge.scope, Some("repository:a/b:pull"));
    }

    #[test]
    fn classifies_redirect_auth_and_retry_responses() {
        let headers = [Header {
            name: "Retry-After",
            value: b"12",
        }];
        assert!(matches!(
            ResponseHead {
                status: 429,
                headers: &headers
            }
            .classify(false),
            Ok(ResponseAction::Retry(advice)) if advice.after == Some(RetryAfter::Seconds(12))
        ));
    }

    #[test]
    fn strips_auth_across_origins_and_rejects_downgrades() {
        let current = Target::parse("https://registry.example/v2/blob").unwrap();
        assert_eq!(
            validate_redirect(current, "https://objects.example/blob", false).unwrap(),
            Redirect {
                target: Target::parse("https://objects.example/blob").unwrap(),
                strip_authorization: true,
            }
        );
        assert!(validate_redirect(current, "http://registry.example/blob", false).is_err());
        assert_eq!(current.scheme, Scheme::Https);
    }

    #[test]
    fn parses_token_alias_and_pagination_link() {
        let token = TokenResponse::parse(br#"{"access_token":"abc","expires_in":60}"#).unwrap();
        assert_eq!(token.token.as_str(), Some("abc"));
        assert_eq!(token.expires_in, Some(60));
        let mut authorization = [0; 32];
        assert_eq!(
            token.bearer_authorization(&mut authorization).unwrap(),
            "Bearer abc"
        );
        let aliases =
            TokenResponse::parse(br#"{"token":"preferred","access_token":"alias"}"#).unwrap();
        let mut authorization = [0; 32];
        assert_eq!(
            aliases.bearer_authorization(&mut authorization).unwrap(),
            "Bearer preferred"
        );

        let headers = [Header {
            name: "Link",
            value: br#"</v2/name/tags/list?n=2&last=b>; rel="next""#,
        }];
        assert_eq!(
            ResponseHead {
                status: 200,
                headers: &headers
            }
            .next_link()
            .unwrap(),
            Some("/v2/name/tags/list?n=2&last=b")
        );
    }

    #[test]
    fn builds_bearer_urls_and_basic_credentials() {
        let AuthChallenge::Bearer(challenge) = AuthChallenge::parse(
            r#"Bearer realm="https://auth.example/token",service="registry.example",scope="repository:team/image:pull""#,
        )
        .unwrap()
        else {
            panic!("expected bearer challenge");
        };
        let mut url = [0; 192];
        assert_eq!(
            bearer_token_url(challenge, &mut url).unwrap(),
            "https://auth.example/token?service=registry.example&scope=repository%3Ateam%2Fimage%3Apull"
        );

        let mut authorization = [0; 64];
        assert_eq!(
            basic_authorization(
                Credentials {
                    username: "user",
                    password: "p@ss",
                },
                &mut authorization,
            )
            .unwrap(),
            "Basic dXNlcjpwQHNz"
        );
    }
}
