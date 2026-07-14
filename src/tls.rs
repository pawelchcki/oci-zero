//! Optional MbedTLS connector for reqwless streams.

use core::{ffi::CStr, net::SocketAddr};

use embedded_io::{Error as _, ErrorKind};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use mbedtls_rs::{
    AuthMode, Certificate, ClientSessionConfig, Session, SessionConfig, SessionError, TlsReference,
    TlsVersion,
};

use crate::{reference::Scheme, registry::Target};

/// Establishes a certificate-verified MbedTLS session.
///
/// The application owns the active `Tls` instance, RNG, CA certificate,
/// server-name storage, and MbedTLS C allocator hooks.
pub async fn connect<'a, T, D>(
    tls: TlsReference<'a>,
    tcp: &'a T,
    dns: &'a D,
    target: Target<'a>,
    server_name: &'a CStr,
    ca_chain: Certificate<'a>,
) -> Result<Session<'a, T::Connection<'a>>, ConnectError>
where
    T: TcpConnect + 'a,
    D: Dns + 'a,
{
    if target.scheme != Scheme::Https {
        return Err(ConnectError::HttpsRequired);
    }
    let (host, port) =
        crate::reqwless::authority(target).map_err(|_| ConnectError::InvalidAuthority)?;
    if server_name.to_bytes() != host.as_bytes() {
        return Err(ConnectError::ServerNameMismatch);
    }
    let address = dns
        .get_host_by_name(host, AddrType::Either)
        .await
        .map_err(|_| ConnectError::Dns)?;
    let stream = tcp
        .connect(SocketAddr::new(address, port))
        .await
        .map_err(|error| ConnectError::Network(error.kind()))?;
    let config = SessionConfig::Client(ClientSessionConfig {
        ca_chain: Some(ca_chain),
        creds: None,
        server_name: Some(server_name),
        auth_mode: AuthMode::Required,
        min_version: TlsVersion::Tls1_2,
        alpn_protocols: None,
    });
    let mut session = Session::new(tls, stream, &config).map_err(ConnectError::Tls)?;
    session.connect().await.map_err(ConnectError::Tls)?;
    let details = session.tls_verification_details();
    if details != 0 {
        return Err(ConnectError::Verification(details));
    }
    Ok(session)
}

#[derive(Debug)]
pub enum ConnectError {
    Dns,
    Network(ErrorKind),
    Tls(SessionError),
    Verification(u32),
    InvalidAuthority,
    HttpsRequired,
    ServerNameMismatch,
}

impl core::fmt::Display for ConnectError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Dns => formatter.write_str("DNS lookup failed"),
            Self::Network(error) => write!(formatter, "network operation failed: {error:?}"),
            Self::Tls(error) => write!(formatter, "TLS operation failed: {error}"),
            Self::Verification(details) => {
                write!(
                    formatter,
                    "TLS certificate verification failed: {details:#x}"
                )
            }
            Self::InvalidAuthority => formatter.write_str("invalid HTTPS authority"),
            Self::HttpsRequired => formatter.write_str("MbedTLS connector requires HTTPS"),
            Self::ServerNameMismatch => {
                formatter.write_str("TLS server name does not match request authority")
            }
        }
    }
}
