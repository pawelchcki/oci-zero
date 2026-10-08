//! Optional reqwless adapter for the transport-neutral registry core.

use core::net::SocketAddr;

use embedded_io::{Error as _, ErrorKind};
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use reqwless::{
    request::{Method as HttpMethod, Request as HttpRequest, RequestBuilder},
    response::Response,
};

use crate::{
    reference::Scheme,
    registry::{Header, Method, Request, ResponseHead, Target},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyAction {
    Read,
    Discard,
}

pub trait ResponseSink {
    type Error;

    fn head(&mut self, head: ResponseHead<'_>) -> Result<BodyAction, Self::Error>;
    fn body(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExchangeResult {
    pub status: u16,
    pub body_size: u64,
}

/// Writes one request and returns its streaming reqwless response.
pub async fn send_on<'connection, 'buffer, C>(
    stream: &'connection mut C,
    request: Request<'_>,
    header_buffer: &'buffer mut [u8],
) -> Result<Response<'connection, 'buffer, C>, reqwless::Error>
where
    C: Read + Write,
{
    let method = match request.method {
        Method::Get => HttpMethod::GET,
        Method::Head => HttpMethod::HEAD,
    };
    let mut headers = [("", ""); 3];
    let mut header_count = 0;
    headers[header_count] = ("Accept", request.accept);
    header_count += 1;
    headers[header_count] = (
        "User-Agent",
        concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
    );
    header_count += 1;
    if let Some(authorization) = request.authorization {
        headers[header_count] = ("Authorization", authorization);
        header_count += 1;
    }
    let encoded = HttpRequest::new(method, request.target.path_and_query)
        .host(request.target.authority)
        .headers(&headers[..header_count])
        .build();
    encoded.write_header(stream).await?;
    stream
        .flush()
        .await
        .map_err(|error| reqwless::Error::Network(error.kind()))?;
    Response::read(stream, method, header_buffer).await
}

/// Sends one planned request over an already connected plain or TLS stream.
pub async fn execute_on<C, S>(
    stream: &mut C,
    request: Request<'_>,
    header_buffer: &mut [u8],
    body_buffer: &mut [u8],
    sink: &mut S,
) -> Result<ExchangeResult, AdapterError<S::Error>>
where
    C: Read + Write,
    S: ResponseSink,
{
    if body_buffer.is_empty() {
        return Err(AdapterError::EmptyBodyBuffer);
    }
    let response = send_on(stream, request, header_buffer)
        .await
        .map_err(AdapterError::Http)?;
    let status = response.status.0;
    let mut parsed = [Header {
        name: "",
        value: &[],
    }; 64];
    let mut count = 0;
    for (name, value) in response.headers() {
        if count == parsed.len() {
            return Err(AdapterError::TooManyHeaders);
        }
        parsed[count] = Header { name, value };
        count += 1;
    }
    let action = sink
        .head(ResponseHead {
            status,
            headers: &parsed[..count],
        })
        .map_err(AdapterError::Sink)?;

    let mut reader = response.body().reader();
    let mut body_size = 0u64;
    loop {
        let length = reader.read(body_buffer).await.map_err(AdapterError::Http)?;
        if length == 0 {
            break;
        }
        body_size = body_size
            .checked_add(length as u64)
            .ok_or(AdapterError::BodyTooLarge)?;
        if action == BodyAction::Read {
            sink.body(&body_buffer[..length])
                .map_err(AdapterError::Sink)?;
        }
    }
    Ok(ExchangeResult { status, body_size })
}

/// Resolves, connects, and executes one plain-HTTP request.
pub async fn execute<T, D, S>(
    tcp: &T,
    dns: &D,
    request: Request<'_>,
    header_buffer: &mut [u8],
    body_buffer: &mut [u8],
    sink: &mut S,
) -> Result<ExchangeResult, AdapterError<S::Error>>
where
    T: TcpConnect,
    D: Dns,
    S: ResponseSink,
{
    if request.target.scheme != Scheme::Http {
        return Err(AdapterError::TlsRequired);
    }
    let (host, port) = authority(request.target).ok_or(AdapterError::InvalidAuthority)?;
    let address = dns
        .get_host_by_name(host, AddrType::Either)
        .await
        .map_err(|_| AdapterError::Dns)?;
    let mut stream = tcp
        .connect(SocketAddr::new(address, port))
        .await
        .map_err(|error| AdapterError::Network(error.kind()))?;
    execute_on(&mut stream, request, header_buffer, body_buffer, sink).await
}

pub(crate) fn authority(target: Target<'_>) -> Option<(&str, u16)> {
    let default_port = match target.scheme {
        Scheme::Http => 80,
        Scheme::Https => 443,
    };
    if let Some(remainder) = target.authority.strip_prefix('[') {
        let bracket = remainder.find(']')?;
        let host = &remainder[..bracket];
        let suffix = &remainder[bracket + 1..];
        let port = if suffix.is_empty() {
            default_port
        } else {
            suffix.strip_prefix(':')?.parse().ok()?
        };
        return Some((host, port));
    }
    match target.authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => {
            Some((host, port.parse().ok()?))
        }
        Some(_) => None,
        None => Some((target.authority, default_port)),
    }
}

#[derive(Debug, derive_more::Display)]
pub enum AdapterError<E> {
    #[display("DNS lookup failed")]
    Dns,
    #[display("network operation failed: {_0:?}")]
    Network(ErrorKind),
    #[display("HTTP operation failed: {_0}")]
    Http(reqwless::Error),
    #[display("response sink failed: {_0}")]
    Sink(E),
    #[display("HTTPS requires the tls feature and connector")]
    TlsRequired,
    #[display("invalid HTTP authority")]
    InvalidAuthority,
    #[display("HTTP body buffer is empty")]
    EmptyBodyBuffer,
    #[display("HTTP response has too many headers")]
    TooManyHeaders,
    #[display("HTTP response body size overflow")]
    BodyTooLarge,
}

#[cfg(test)]
mod tests {
    use super::authority;
    use crate::{reference::Scheme, registry::Target};

    #[test]
    fn parses_authorities() {
        assert_eq!(
            authority(Target {
                scheme: Scheme::Https,
                authority: "registry.example:8443",
                path_and_query: "/",
            })
            .unwrap(),
            ("registry.example", 8443)
        );
        assert_eq!(
            authority(Target {
                scheme: Scheme::Http,
                authority: "[::1]:5000",
                path_and_query: "/",
            })
            .unwrap(),
            ("::1", 5000)
        );
    }
}
