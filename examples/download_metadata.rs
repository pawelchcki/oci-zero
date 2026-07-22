use std::env;
use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use oci_zero::digest::Verifier;
use oci_zero::docker_credentials::DockerCredentialProvider;
use oci_zero::metadata::{DescriptorIter, Document, DocumentKind};
use oci_zero::reference::{Reference, Scheme, Selector};
use oci_zero::registry::{
    basic_authorization, bearer_token_url, AuthChallenge, BearerChallenge, Credentials, Request,
    RequestPlanner, Target, TokenResponse,
};

const REFERENCE: &str = "oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9";
const MAX_METADATA_SIZE: u64 = 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let input = env::args().nth(1).unwrap_or_else(|| REFERENCE.to_owned());
    let reference = checked(Reference::parse(&input))?;
    let expected_digest = match reference.selector() {
        Selector::Digest(digest) => Some(digest),
        Selector::Tag(_) => None,
    };

    let planner = RequestPlanner::new(reference);
    let mut path = [0; 512];
    let request = checked(planner.manifest(&mut path))?;
    let agent = ureq::AgentBuilder::new().build();
    let mut credentials = DockerCredentialProvider::from_env()?;
    let response = get_with_auth(&agent, request, &mut credentials)?;
    let content_type = response.header("Content-Type").map(str::to_owned);
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_SIZE {
        return Err(invalid_data("manifest exceeds the example's 1 MiB limit").into());
    }

    if let Some(digest) = expected_digest {
        let mut verifier = Verifier::digest_only(digest);
        checked(verifier.update(&bytes))?;
        checked(verifier.finish())?;
    }

    let document = checked(Document::parse(&bytes))?;
    let (kind, children) = match document.kind() {
        DocumentKind::Index => (
            "index",
            descriptor_count(
                checked(document.index())?
                    .manifests()
                    .map_err(invalid_data)?,
            )?,
        ),
        DocumentKind::Manifest => (
            "manifest",
            descriptor_count(
                checked(document.manifest())?
                    .layers()
                    .map_err(invalid_data)?,
            )?,
        ),
    };
    let selector = expected_digest
        .map(|digest| digest.to_string())
        .unwrap_or_else(|| "mutable tag".to_owned());
    println!(
        "downloaded {kind} {selector} ({} bytes, {} entries, {})",
        bytes.len(),
        children,
        content_type.as_deref().unwrap_or("unknown media type")
    );
    Ok(())
}

fn get_with_auth(
    agent: &ureq::Agent,
    request: Request<'_>,
    credentials: &mut DockerCredentialProvider,
) -> Result<ureq::Response, Box<dyn Error>> {
    let url = format!(
        "{}://{}{}",
        request.target.scheme, request.target.authority, request.target.path_and_query
    );
    let mut authorization: Option<String> = None;
    for attempt in 0..2 {
        let mut host_request = agent.get(&url).set("Accept", request.accept).set(
            "User-Agent",
            concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
        );
        if let Some(value) = &authorization {
            host_request = host_request.set("Authorization", value);
        }
        match host_request.call() {
            Ok(response) => return Ok(response),
            Err(ureq::Error::Status(401, response)) if attempt == 0 => {
                let value = response
                    .header("WWW-Authenticate")
                    .ok_or_else(|| invalid_data("registry returned 401 without a challenge"))?;
                authorization = Some(match checked(AuthChallenge::parse(value))? {
                    AuthChallenge::Bearer(challenge) => fetch_bearer_authorization(
                        agent,
                        request.target.authority,
                        challenge,
                        credentials,
                    )?,
                    AuthChallenge::Basic { .. } => {
                        basic_from_docker_config(request.target.authority, credentials)?
                            .ok_or_else(|| {
                                invalid_data(
                                    "registry requires credentials; run docker login first",
                                )
                            })?
                    }
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(invalid_data("registry rejected the configured Docker credential").into())
}

fn fetch_bearer_authorization(
    agent: &ureq::Agent,
    authority: &str,
    challenge: BearerChallenge<'_>,
    credentials: &mut DockerCredentialProvider,
) -> Result<String, Box<dyn Error>> {
    let mut url = [0; 4096];
    let url = checked(bearer_token_url(challenge, &mut url))?;
    if checked(Target::parse(url))?.scheme != Scheme::Https {
        return Err(invalid_data("registry requested an insecure token endpoint").into());
    }
    let mut request = agent.get(url).set("Accept", "application/json").set(
        "User-Agent",
        concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
    );
    if let Some(authorization) = basic_from_docker_config(authority, credentials)? {
        request = request.set("Authorization", &authorization);
    }
    let response = request.call()?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_SIZE {
        return Err(invalid_data("token response exceeds the example's 1 MiB limit").into());
    }
    let token = checked(TokenResponse::parse(&bytes))?;
    let mut authorization = vec![0; bytes.len() + "Bearer ".len()];
    Ok(checked(token.bearer_authorization(&mut authorization))?.to_owned())
}

fn basic_from_docker_config(
    authority: &str,
    provider: &mut DockerCredentialProvider,
) -> Result<Option<String>, Box<dyn Error>> {
    let Some(credentials) = provider.credentials_for(authority)? else {
        return Ok(None);
    };
    let encoded_length = 4 * (credentials.username.len() + credentials.password.len() + 3) / 3;
    let mut authorization = vec![0; "Basic ".len() + encoded_length];
    let authorization = checked(basic_authorization(
        Credentials {
            username: credentials.username,
            password: credentials.password,
        },
        &mut authorization,
    ))?;
    Ok(Some(authorization.to_owned()))
}

fn descriptor_count(mut descriptors: DescriptorIter<'_>) -> io::Result<usize> {
    descriptors.try_fold(0, |count, descriptor| {
        checked(descriptor).map(|_| count + 1)
    })
}

fn checked<T, E: fmt::Display>(result: Result<T, E>) -> io::Result<T> {
    result.map_err(invalid_data)
}

fn invalid_data(error: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
