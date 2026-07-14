use core::ffi::CStr;

use embedded_io_async::Read;
use mbedtls_rs::{Certificate, Tls, X509};
use oci_zero::registry::{bearer_token_url, AuthChallenge, Method, Request, Target, TokenResponse};

use crate::platform::{DnsResolver, OsRng, TcpStack};
use crate::{extract_reader, Failure, Fixture};

const HEADER_BUFFER_SIZE: usize = 8 * 1024;
const TOKEN_BUFFER_SIZE: usize = 8 * 1024;
const TOKEN_URL_SIZE: usize = 4096;
// The pinned Datadog endpoint chains to DigiCert Global Root G2. GHCR chains
// to Sectigo Public Server Authentication Root R46, cross-signed by USERTrust.
const ROOT_CERTIFICATES: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
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
-----END CERTIFICATE-----\n\
-----BEGIN CERTIFICATE-----\n\
MIIGlTCCBH2gAwIBAgIRANJ/u8HeNZ5SFq1hSVhgmcQwDQYJKoZIhvcNAQEMBQAw\n\
gYgxCzAJBgNVBAYTAlVTMRMwEQYDVQQIEwpOZXcgSmVyc2V5MRQwEgYDVQQHEwtK\n\
ZXJzZXkgQ2l0eTEeMBwGA1UEChMVVGhlIFVTRVJUUlVTVCBOZXR3b3JrMS4wLAYD\n\
VQQDEyVVU0VSVHJ1c3QgUlNBIENlcnRpZmljYXRpb24gQXV0aG9yaXR5MB4XDTIx\n\
MDMyMjAwMDAwMFoXDTM4MDExODIzNTk1OVowXzELMAkGA1UEBhMCR0IxGDAWBgNV\n\
BAoTD1NlY3RpZ28gTGltaXRlZDE2MDQGA1UEAxMtU2VjdGlnbyBQdWJsaWMgU2Vy\n\
dmVyIEF1dGhlbnRpY2F0aW9uIFJvb3QgUjQ2MIICIjANBgkqhkiG9w0BAQEFAAOC\n\
Ag8AMIICCgKCAgEAk77VNlJ12AEjoBxHQknuY7a3If3EldVIKyZ8FFMQ2nn9K7ct\n\
pNQs+uoy3UnCub0PSD17WphUr55dMXRPB/xQId2kz2hPGxJjbSWZTCqZ80gwYfqB\n\
fB6nCErcPiscHxhMcao1jK34bug7StnllALWiYQTqm3ITzPMUJY3kjPcX4jnn1TZ\n\
SPCYQ9Zm/Z8XOEPFAVEL1+MjDxRdWxTnS77d9MjaAzfR1jmhIVEwg7Bt1zBOlluR\n\
8HAkq79FgWRDDb0hOi886Z4NyyC1QifM2m+b7mQwkDnNk2WBITG1I1AzNyLjOO34\n\
MTDMRf5i+dFdMnlCh99qzFYZQE3Oqrv5tXZJlPEn+JGlg+UGs2MOgNzgElWApjtm\n\
tDmHLcjw0NEU6eQNTQ72XVdyxTscR1ad4tX7gWGMzE2AkDRbt9cUddzYBEifwMEo\n\
iLTpHMqnsfFWt3tJTFnlIBWohAIp+jiUaZpJBo/NH3kUFxIMg3reH7GX7vmXeCik\n\
yESS6X0mBaZYcpt5E9gRX67FOGI0aLKGMI74kGGeMmz1BzbNokxu7Io27fLmmRVE\n\
cMN8vJw5wLTha/eDJSNX2RKA5UnwdQ/vjescm1QotCE8/HwK/+97a3X/ix2gGQWr\n\
+vgrgULoOLq7+6r9PeDzyt9Ol5cp7fMYVumllqy9w5CYsuD5otSmR0N8bc8CAwEA\n\
AaOCASAwggEcMB8GA1UdIwQYMBaAFFN5v1qqK0rPVIDh2JvAnfKyA2bLMB0GA1Ud\n\
DgQWBBRWc1hklfmSGrASKgRieaFAFYghSTAOBgNVHQ8BAf8EBAMCAYYwDwYDVR0T\n\
AQH/BAUwAwEB/zAdBgNVHSUEFjAUBggrBgEFBQcDAQYIKwYBBQUHAwIwEQYDVR0g\n\
BAowCDAGBgRVHSAAMFAGA1UdHwRJMEcwRaBDoEGGP2h0dHA6Ly9jcmwudXNlcnRy\n\
dXN0LmNvbS9VU0VSVHJ1c3RSU0FDZXJ0aWZpY2F0aW9uQXV0aG9yaXR5LmNybDA1\n\
BggrBgEFBQcBAQQpMCcwJQYIKwYBBQUHMAGGGWh0dHA6Ly9vY3NwLnVzZXJ0cnVz\n\
dC5jb20wDQYJKoZIhvcNAQEMBQADggIBADpvBIlq7bMU0cFDT/9P9+BsgCkRgQs0\n\
S6Bf7vJSlWMHwby0VGvxCS0hrbi0K2BINZbEbsVsgpQq04431yyoVn3Hldorgq24\n\
RldRDOOipEZDTFB9wC9HYt1thHF00XeG2C8KC1plwoEzKAIhPvefI/C3cT0CfTXJ\n\
uFjUbKIgSwjNjw6YHtLgoy/hd5+JLUlLco/gzFX/qWbT7tEquOMYpsNKWZj8TLqP\n\
q6zMiG4Na6feEZte6YPXGrMWlTWN341vDedc+yxQqSug79HJUQcOZs7KyDWztmae\n\
QxsPE49UV/8XwrfZtZaYyrs4FpD94Z4Q8dzXGL8+qEJjxgcza7W6PROaClubavd1\n\
VKPm8+aCW77u7SxpR2TFGL6kPdxsKyFijpcunR5V79sUyROfNdzjrAcFWZXK8sbb\n\
9FlnwuVG677JLv+ZVTX5AxLvW5OB4zt5uS+zB62wJ/Wv+jXGAttSAcJec4iFgCWH\n\
Rvdi/jJoSzRLa3nEzx6pFIzclSCnh0u1xCeLcUBypSiPga8W+6PkuoyQq8U9qs9E\n\
oxG5NvrvlyshwUS9yvcZRGw7Ljlx4jJH/BhIPR8kIBCQj1vna9TziZOrw1Of8hDU\n\
bHKFG9Pm8Dp2vbjz/2JH39qvxshPKVllGfq+5klPm7yZRUYTiCMAbqwNdL/nsqF2\n\
Rnnyp58XRStJ\n\
-----END CERTIFICATE-----\n\0";

pub async fn extract_https(url: &str, target: &[u8], fixture: Fixture) -> Result<(), Failure> {
    let request_target = Target::parse(url).map_err(|_| Failure::Usage)?;
    let server_name = ServerName::parse(request_target.authority)?;

    let resolver = DnsResolver::google();
    let mut random = OsRng::open().map_err(|_| Failure::Input)?;
    // SAFETY: `tls` and every session referencing it are dropped before
    // `random` at the end of this function.
    let tls = unsafe { Tls::new_local_borrows(&mut random) }.map_err(|_| Failure::Tls)?;
    let mut authorization = [0u8; TOKEN_BUFFER_SIZE + "Bearer ".len()];
    let authorization = if request_target.authority.eq_ignore_ascii_case("ghcr.io") {
        Some(
            fetch_bearer_authorization(
                tls.reference(),
                &resolver,
                request_target,
                &mut authorization,
            )
            .await?,
        )
    } else {
        None
    };
    let mut session = oci_zero::tls::connect(
        tls.reference(),
        &TcpStack,
        &resolver,
        request_target,
        server_name.as_cstr()?,
        root_certificate()?,
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
                authorization,
            },
            &mut headers,
        )
        .await
        .map_err(|_| Failure::Http)?;
        if response.status.0 != 200 {
            return Err(Failure::Http);
        }

        let mut body = response.body().reader();
        extract_reader(&mut body, target, fixture).await?;
    }
    session.close().await.map_err(|_| Failure::Tls)?;
    Ok(())
}

async fn fetch_bearer_authorization<'buffer>(
    tls: mbedtls_rs::TlsReference<'_>,
    resolver: &DnsResolver,
    target: Target<'_>,
    authorization: &'buffer mut [u8],
) -> Result<&'buffer str, Failure> {
    let mut token_url = [0u8; TOKEN_URL_SIZE];
    let token_url_length = {
        let server_name = ServerName::parse(target.authority)?;
        let mut session = oci_zero::tls::connect(
            tls,
            &TcpStack,
            resolver,
            target,
            server_name.as_cstr()?,
            root_certificate()?,
        )
        .await
        .map_err(|_| Failure::Tls)?;
        let length = {
            let mut headers = [0u8; HEADER_BUFFER_SIZE];
            let response = oci_zero::reqwless::send_on(
                &mut session,
                Request {
                    method: Method::Get,
                    target,
                    accept: "*/*",
                    authorization: None,
                },
                &mut headers,
            )
            .await
            .map_err(|_| Failure::Http)?;
            if response.status.0 != 401 {
                return Err(Failure::Http);
            }
            let challenge = response
                .headers()
                .find(|(name, _)| name.eq_ignore_ascii_case("WWW-Authenticate"))
                .ok_or(Failure::Http)?;
            let challenge = core::str::from_utf8(challenge.1).map_err(|_| Failure::Http)?;
            let AuthChallenge::Bearer(challenge) =
                AuthChallenge::parse(challenge).map_err(|_| Failure::Http)?
            else {
                return Err(Failure::Http);
            };
            let url = bearer_token_url(challenge, &mut token_url).map_err(|_| Failure::Http)?;
            let length = url.len();
            drain(response.body().reader()).await?;
            length
        };
        session.close().await.map_err(|_| Failure::Tls)?;
        length
    };

    let token_url =
        core::str::from_utf8(&token_url[..token_url_length]).map_err(|_| Failure::Http)?;
    let token_target = Target::parse(token_url).map_err(|_| Failure::Http)?;
    let server_name = ServerName::parse(token_target.authority)?;
    let mut session = oci_zero::tls::connect(
        tls,
        &TcpStack,
        resolver,
        token_target,
        server_name.as_cstr()?,
        root_certificate()?,
    )
    .await
    .map_err(|_| Failure::Tls)?;
    let mut token_body = [0u8; TOKEN_BUFFER_SIZE + 1];
    let token_length = {
        let mut headers = [0u8; HEADER_BUFFER_SIZE];
        let response = oci_zero::reqwless::send_on(
            &mut session,
            Request {
                method: Method::Get,
                target: token_target,
                accept: "application/json",
                authorization: None,
            },
            &mut headers,
        )
        .await
        .map_err(|_| Failure::Http)?;
        if response.status.0 != 200 {
            return Err(Failure::Http);
        }
        read_bounded(response.body().reader(), &mut token_body).await?
    };
    session.close().await.map_err(|_| Failure::Tls)?;
    let token = TokenResponse::parse(&token_body[..token_length]).map_err(|_| Failure::Http)?;
    token
        .bearer_authorization(authorization)
        .map_err(|_| Failure::Http)
}

fn root_certificate() -> Result<Certificate<'static>, Failure> {
    let root = CStr::from_bytes_with_nul(ROOT_CERTIFICATES).map_err(|_| Failure::Tls)?;
    Certificate::new(X509::PEM(root)).map_err(|_| Failure::Tls)
}

async fn drain<R: Read>(mut reader: R) -> Result<(), Failure> {
    let mut buffer = [0u8; 1024];
    while reader.read(&mut buffer).await.map_err(|_| Failure::Http)? != 0 {}
    Ok(())
}

async fn read_bounded<R: Read>(mut reader: R, buffer: &mut [u8]) -> Result<usize, Failure> {
    let mut length = 0;
    while length < buffer.len() {
        let read = reader
            .read(&mut buffer[length..])
            .await
            .map_err(|_| Failure::Http)?;
        if read == 0 {
            return Ok(length);
        }
        length += read;
    }
    Err(Failure::Http)
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
