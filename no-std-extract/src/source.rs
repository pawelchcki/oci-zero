use core::ffi::CStr;

use mbedtls_rs::{Certificate, Tls, X509};
use oci_zero::registry::{Method, Request, Target};

use crate::platform::{DnsResolver, OsRng, TcpStack};
use crate::{extract_reader, Failure};

const HEADER_BUFFER_SIZE: usize = 8 * 1024;
const ROOT_CERTIFICATE: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIDjjCCAnagAwIBAgIQAzrx5qcRqaC7KGSxHQn65TANBgkqhkiG9w0BAQsFADBh\n\
MQswCQYDVQQGEwJVUzEVMBMGA1UEChMMRGlnaUNlcnQgSW5jMRkwFwYDVQQLExB3\n\
d3cuZGlnaUNlcnQuY29tMSAwHgYDVQQDExdEaWdpQ2VydCBHbG9iYWwgUm9vdCBH\n\
MjAeFw0xMzA4MDExMjAwMDBaFw0zODAxMTUxMjAwMDBaMGExCzAJBgNVBAYTAlVT\n\
MRUwEwYDVQQKEwxEaWdpQ2VydCBJbmMxGTAXBgNVBAsTEHd3dy5kaWdpY2VydC5j\n\
b20xIDAeBgNVBAMTF0RpZ2lDZXJ0IEdsb2JhbCBSb290IEcyMIIBIjANBgkqhkiG\n\
9w0BAQEFAAOCAQ8AMIIBCgKCAQEAuzfNNNx7a8myaJCtSnX/RrohCgiN9RlUyfuI\n\
2/Ou8jqJkTx65qsGGmvPrC3oXgkkRLpimn7Wo6h+4FR1IAWsULecYxpsMNzaHxmx\n\
1x7e/dfgy5SDN67sH0NO3Xss0r0upS/kqbitOtSZpLYl6ZtrAGCSYP9PIUkY92eQ\n\
q2EGnI/yuum06ZIya7XzV+hdG82MHauVBJVJ8zUtluNJbd134/tJS7SsVQepj5Wz\n\
tCO7TG1F8PapspUwtP1MVYwnSlcUfIKdzXOS0xZKBgyMUNGPHgm+F6HmIcr9g+UQ\n\
vIOlCsRnKPZzFBQ9RnbDhxSJITRNrw9FDKZJobq7nMWxM4MphQIDAQABo0IwQDAP\n\
BgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIBhjAdBgNVHQ4EFgQUTiJUIBiV\n\
5uNu5g/6+rkS7QYXjzkwDQYJKoZIhvcNAQELBQADggEBAGBnKJRvDkhj6zHd6mcY\n\
1Yl9PMWLSn/pvtsrF9+wX3N3KjITOYFnQoQj8kVnNeyIv/iPsGEMNKSuIEyExtv4\n\
NeF22d+mQrvHRAiGfzZ0JFrabA0UWTW98kndth/Jsw1HKj2ZL7tcu7XUIOGZX1NG\n\
Fdtom/DzMNU+MeKNhJ7jitralj41E6Vf8PlwUHBHQRFXGU7Aj64GxJUTFy8bJZ91\n\
8rGOmaFvE7FBcf6IKshPECBV1/MUReXgRPTqh5Uykw7+U0b6LJ3/iyK5S9kJRaTe\n\
pLiaWN0bfVKfjllDiIGknibVb63dDcY3fe0Dkhvld1927jyNxF1WW6LZZm6zNTfl\n\
MrY=\n\
-----END CERTIFICATE-----\n\0";

pub async fn extract_https(url: &str, target: &[u8]) -> Result<(), Failure> {
    let request_target = Target::parse(url).map_err(|_| Failure::Usage)?;
    let server_name = ServerName::parse(request_target.authority)?;
    let root = CStr::from_bytes_with_nul(ROOT_CERTIFICATE).map_err(|_| Failure::Tls)?;
    let certificate = Certificate::new(X509::PEM(root)).map_err(|_| Failure::Tls)?;

    let resolver = DnsResolver::google();
    let mut random = OsRng::open().map_err(|_| Failure::Input)?;
    // SAFETY: `tls` and every session referencing it are dropped before
    // `random` at the end of this function.
    let tls = unsafe { Tls::new_local_borrows(&mut random) }.map_err(|_| Failure::Tls)?;
    let mut session = oci_zero::tls::connect(
        tls.reference(),
        &TcpStack,
        &resolver,
        request_target,
        server_name.as_cstr()?,
        certificate,
    )
    .await
    .map_err(|_| Failure::Tls)?;

    {
        let mut headers = [0u8; HEADER_BUFFER_SIZE];
        let response = oci_zero::reqwless::send_on(
            &mut session,
            Request {
                method: Method::Get,
                target: request_target,
                accept: "*/*",
                authorization: None,
            },
            &mut headers,
        )
        .await
        .map_err(|_| Failure::Http)?;
        if response.status.0 != 200 {
            return Err(Failure::Http);
        }

        let mut body = response.body().reader();
        extract_reader(&mut body, target).await?;
    }
    session.close().await.map_err(|_| Failure::Tls)?;
    Ok(())
}

struct ServerName {
    server_name: [u8; 256],
}

impl ServerName {
    fn parse(authority: &str) -> Result<Self, Failure> {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                let port = port.parse::<u16>().map_err(|_| Failure::Usage)?;
                (host, port)
            }
            _ => (authority, 443),
        };
        if host.len() >= 256 {
            return Err(Failure::Usage);
        }
        let mut server_name = [0u8; 256];
        server_name[..host.len()].copy_from_slice(host.as_bytes());
        let _ = port;
        Ok(Self { server_name })
    }

    fn as_cstr(&self) -> Result<&CStr, Failure> {
        CStr::from_bytes_until_nul(&self.server_name).map_err(|_| Failure::Usage)
    }
}
