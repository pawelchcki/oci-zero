use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use oci_zero::digest::{Digest, Verifier};
use oci_zero::metadata::{
    Descriptor, DescriptorIter, Document, DocumentKind, ImageManifest, JsonString,
    OCI_INDEX_MEDIA_TYPE, OCI_MANIFEST_MEDIA_TYPE,
};
use oci_zero::reference::{Reference, Selector};
use oci_zero::registry::{
    bearer_token_url, AuthChallenge, BearerChallenge, Request, RequestPlanner, TokenResponse,
};

const DEFAULT_REFERENCE: &str = "oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9";
const REFERRERS_REFERENCE: &str = "oci://ghcr.io/cri-o/bundle@sha256:3d2144b7e80c056a5bc24c84626e78e9441729c9ec2f3e65bfb3fd2977b8974d";
const SMOKE_REFERENCES: &[&str] = &[
    DEFAULT_REFERENCE,
    "oci://ghcr.io/prometheus-community/charts/prometheus@sha256:523ae68ca30de9c04b04afb8c70526227dd16278e915198876423cc29b0d2bb5",
    "oci://registry-1.docker.io/bitnamicharts/harbor@sha256:e262907a6dae5c51d268fedf0e6d528029d5314bae92a61aed1657f04e6ee681",
    REFERRERS_REFERENCE,
    "oci://ghcr.io/oras-project/oras@sha256:46c55ba0eac848ade573594ef23fa5b1784227f151dab6eac38de2afd0b9c382",
    "oci://registry.k8s.io/pause@sha256:ee6521f290b2168b6e0935a181d4cff9be1ac3f505666ef0e3c98fae8199917a",
];
const MAX_METADATA_SIZE: u64 = 1024 * 1024;

struct RegistryClient<'a> {
    reference: Reference<'a>,
    agent: ureq::Agent,
    authorization: Option<String>,
}

impl<'a> RegistryClient<'a> {
    fn new(reference: Reference<'a>) -> Self {
        Self {
            reference,
            agent: ureq::AgentBuilder::new().build(),
            authorization: None,
        }
    }

    fn root_manifest(&mut self) -> Result<FetchedBody, Box<dyn Error>> {
        let planner = RequestPlanner::new(self.reference);
        let mut path = [0; 512];
        let request = checked(planner.manifest(&mut path))?;
        self.get(request, false)?
            .ok_or_else(|| invalid_data("required root manifest was not found").into())
    }

    fn manifest(&mut self, digest: Digest) -> Result<FetchedBody, Box<dyn Error>> {
        let planner = RequestPlanner::new(self.reference);
        let mut path = [0; 512];
        let request = checked(planner.manifest_by_digest(digest, &mut path))?;
        self.get(request, false)?
            .ok_or_else(|| invalid_data("required manifest was not found").into())
    }

    fn blob(&mut self, descriptor: Descriptor<'_>) -> Result<FetchedBody, Box<dyn Error>> {
        let planner = RequestPlanner::new(self.reference);
        let mut path = [0; 512];
        let accept = descriptor.media_type().as_str().unwrap_or("*/*");
        let request = checked(planner.blob(descriptor.digest(), accept, &mut path))?;
        self.get(request, false)?
            .ok_or_else(|| invalid_data("required blob was not found").into())
    }

    fn referrers(&mut self, digest: Digest) -> Result<(FetchedBody, bool), Box<dyn Error>> {
        let planner = RequestPlanner::new(self.reference);
        let mut path = [0; 512];
        let request = checked(planner.referrers(digest, &mut path))?;
        if let Some(body) = self.get(request, true)? {
            return Ok((body, false));
        }

        let mut path = [0; 512];
        let request = checked(planner.referrers_fallback(digest, &mut path))?;
        let body = self
            .get(request, false)?
            .ok_or_else(|| invalid_data("referrers fallback was not found"))?;
        Ok((body, true))
    }

    fn get(
        &mut self,
        request: Request<'_>,
        allow_not_found: bool,
    ) -> Result<Option<FetchedBody>, Box<dyn Error>> {
        let url = format!(
            "{}://{}{}",
            request.target.scheme, request.target.authority, request.target.path_and_query
        );
        for attempt in 0..2 {
            let mut host_request = self
                .agent
                .get(&url)
                .set("Accept", request.accept)
                .set("User-Agent", user_agent());
            if let Some(authorization) = &self.authorization {
                host_request = host_request.set("Authorization", authorization);
            }
            match host_request.call() {
                Ok(response) => return Ok(Some(read_response(response, &url)?)),
                Err(ureq::Error::Status(401, response)) if attempt == 0 => {
                    let value = response
                        .header("WWW-Authenticate")
                        .ok_or_else(|| invalid_data("registry returned 401 without a challenge"))?;
                    let challenge = checked(AuthChallenge::parse(value))?;
                    let AuthChallenge::Bearer(challenge) = challenge else {
                        return Err(invalid_data(
                            "anonymous example supports only Bearer authentication",
                        )
                        .into());
                    };
                    self.authorization = Some(fetch_bearer_authorization(&self.agent, challenge)?);
                }
                Err(ureq::Error::Status(404, _)) if allow_not_found => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        Err(invalid_data("registry rejected the Bearer token").into())
    }
}

struct FetchedBody {
    bytes: Vec<u8>,
    content_type: Option<String>,
}

fn read_response(response: ureq::Response, url: &str) -> Result<FetchedBody, Box<dyn Error>> {
    let content_type = response.header("Content-Type").map(str::to_owned);
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_SIZE {
        return Err(invalid_data(format!(
            "metadata response from {url} exceeds {MAX_METADATA_SIZE} bytes"
        ))
        .into());
    }
    Ok(FetchedBody {
        bytes,
        content_type,
    })
}

fn fetch_bearer_authorization(
    agent: &ureq::Agent,
    challenge: BearerChallenge<'_>,
) -> Result<String, Box<dyn Error>> {
    let mut url = [0; 4096];
    let url = checked(bearer_token_url(challenge, &mut url))?;
    let response = agent
        .get(url)
        .set("Accept", "application/json")
        .set("User-Agent", user_agent())
        .call()?;
    let body = read_response(response, url)?;
    let token = checked(TokenResponse::parse(&body.bytes))?;
    let mut authorization = vec![0; body.bytes.len() + "Bearer ".len()];
    Ok(checked(token.bearer_authorization(&mut authorization))?.to_owned())
}

fn inspect(input: &str, discover_referrers: bool) -> Result<(), Box<dyn Error>> {
    let reference = checked(Reference::parse(input))?;
    let root_digest = match reference.selector() {
        Selector::Digest(digest) => Some(digest),
        Selector::Tag(_) => None,
    };
    let mut client = RegistryClient::new(reference);
    let root = client.root_manifest()?;
    if let Some(digest) = root_digest {
        verify(&root.bytes, digest, None)?;
    }

    let document = checked(Document::parse(&root.bytes))?;
    match document.kind() {
        DocumentKind::Index => {
            let index = checked(document.index())?;
            let media_type = optional_text(checked(index.media_type())?)
                .or(root.content_type.as_deref())
                .unwrap_or(OCI_INDEX_MEDIA_TYPE);
            let count = descriptor_count(checked(index.manifests())?)?;
            println!("{media_type} (schema 2, {count} manifests)");
            for descriptor in checked(index.manifests())? {
                let descriptor = checked(descriptor)?;
                println!(
                    "\n{}: {} ({} bytes, {})",
                    platform_name(descriptor)?,
                    descriptor.digest(),
                    descriptor.size(),
                    text(descriptor.media_type())?
                );
                if let Some(artifact_type) = checked(descriptor.artifact_type())? {
                    println!("  artifact type: {}", text(artifact_type)?);
                }
                let child = client.manifest(descriptor.digest())?;
                verify_descriptor(&child.bytes, descriptor)?;
                let child_document = checked(Document::parse(&child.bytes))?;
                if child_document.kind() != DocumentKind::Manifest {
                    return Err(invalid_data("nested OCI indexes are not supported").into());
                }
                inspect_manifest(
                    &mut client,
                    checked(child_document.manifest())?,
                    child.content_type.as_deref(),
                )?;
            }
        }
        DocumentKind::Manifest => inspect_manifest(
            &mut client,
            checked(document.manifest())?,
            root.content_type.as_deref(),
        )?,
    }
    if discover_referrers {
        let digest = root_digest.ok_or_else(|| {
            invalid_data("referrer discovery example requires a digest reference")
        })?;
        inspect_referrers(&mut client, digest)?;
    }
    Ok(())
}

fn inspect_manifest(
    client: &mut RegistryClient<'_>,
    manifest: ImageManifest<'_>,
    response_media_type: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let media_type = optional_text(checked(manifest.media_type())?)
        .or(response_media_type)
        .unwrap_or(OCI_MANIFEST_MEDIA_TYPE);
    println!("  manifest: schema 2, {media_type}");
    if let Some(artifact_type) = checked(manifest.artifact_type())? {
        println!("  artifact type: {}", text(artifact_type)?);
    }
    if let Some(subject) = checked(manifest.subject())? {
        println!(
            "  subject: {} ({} bytes, {})",
            subject.digest(),
            subject.size(),
            text(subject.media_type())?
        );
        let subject_body = client.manifest(subject.digest())?;
        verify_descriptor(&subject_body.bytes, subject)?;
    }

    for annotation in checked(manifest.annotations())? {
        let (key, value) = checked(annotation)?;
        let key = text(key)?;
        if matches!(
            key.as_str(),
            "org.opencontainers.image.title"
                | "org.opencontainers.image.version"
                | "org.opencontainers.image.description"
                | "com.datadoghq.package.name"
                | "com.datadoghq.package.version"
                | "com.datadoghq.package.size"
        ) {
            println!("  {key}: {}", text(value)?);
        }
    }

    let config_descriptor = checked(manifest.config())?;
    let config = client.blob(config_descriptor)?;
    verify_descriptor(&config.bytes, config_descriptor)?;
    println!(
        "  config: {} ({} bytes, {})",
        config_descriptor.digest(),
        config_descriptor.size(),
        text(config_descriptor.media_type())?
    );
    if let Ok(document) = std::str::from_utf8(&config.bytes) {
        println!("  config document: {document}");
    }

    for layer in checked(manifest.layers())? {
        let layer = checked(layer)?;
        let mut title = None;
        for annotation in checked(layer.annotations())? {
            let (key, value) = checked(annotation)?;
            if text(key)? == "org.opencontainers.image.title" {
                title = Some(text(value)?);
            }
        }
        println!(
            "  skipped layer: {} ({} bytes, {}{})",
            layer.digest(),
            layer.size(),
            text(layer.media_type())?,
            title.map_or_else(String::new, |title| format!(", {title}"))
        );
    }
    Ok(())
}

fn inspect_referrers(
    client: &mut RegistryClient<'_>,
    subject_digest: Digest,
) -> Result<(), Box<dyn Error>> {
    let (body, used_fallback) = client.referrers(subject_digest)?;
    let document = checked(Document::parse(&body.bytes))?;
    let index = checked(document.index())?;
    let count = descriptor_count(checked(index.manifests())?)?;
    let source = if used_fallback {
        "referrers tag fallback"
    } else {
        "Referrers API"
    };
    println!("\n{source}: {count} referrers for {subject_digest}");
    if count == 0 {
        return Err(invalid_data("expected at least one public referrer").into());
    }

    for descriptor in checked(index.manifests())? {
        let descriptor = checked(descriptor)?;
        let child = client.manifest(descriptor.digest())?;
        verify_descriptor(&child.bytes, descriptor)?;
        let document = checked(Document::parse(&child.bytes))?;
        let manifest = checked(document.manifest())?;
        let subject = checked(manifest.subject())?
            .ok_or_else(|| invalid_data("referrer manifest is missing its subject"))?;
        if subject.digest() != subject_digest {
            return Err(invalid_data(format!(
                "referrer subject mismatch: expected {subject_digest}, got {}",
                subject.digest()
            ))
            .into());
        }
        let config = checked(manifest.config())?;
        let artifact_type = checked(manifest.artifact_type())?
            .map(text)
            .transpose()?
            .unwrap_or(text(config.media_type())?);
        if let Some(advertised) = checked(descriptor.artifact_type())? {
            let advertised = text(advertised)?;
            if advertised != artifact_type {
                println!(
                    "  fallback descriptor advertised {advertised}, manifest declares {artifact_type}"
                );
            }
        }
        println!(
            "\nreferrer: {} ({} bytes, {artifact_type})",
            descriptor.digest(),
            descriptor.size()
        );
        inspect_manifest(client, manifest, child.content_type.as_deref())?;
    }
    Ok(())
}

fn verify_descriptor(bytes: &[u8], descriptor: Descriptor<'_>) -> io::Result<()> {
    verify(bytes, descriptor.digest(), Some(descriptor.size()))
}

fn verify(bytes: &[u8], digest: Digest, size: Option<u64>) -> io::Result<()> {
    let mut verifier = size.map_or_else(
        || Verifier::digest_only(digest),
        |size| Verifier::new(digest, size),
    );
    checked(verifier.update(bytes))?;
    checked(verifier.finish())
}

fn descriptor_count(iter: DescriptorIter<'_>) -> io::Result<usize> {
    iter.map(|descriptor| checked(descriptor).map(|_| ()))
        .count_result()
}

trait CountResult: Iterator<Item = io::Result<()>> + Sized {
    fn count_result(mut self) -> io::Result<usize> {
        self.try_fold(0, |count, item| item.map(|()| count + 1))
    }
}

impl<I: Iterator<Item = io::Result<()>>> CountResult for I {}

fn platform_name(descriptor: Descriptor<'_>) -> io::Result<String> {
    let Some(platform) = checked(descriptor.platform())? else {
        return Ok("artifact".to_owned());
    };
    let os = text(platform.os())?;
    let architecture = text(platform.architecture())?;
    Ok(match checked(platform.variant())? {
        Some(variant) => format!("{os}/{architecture}/{}", text(variant)?),
        None => format!("{os}/{architecture}"),
    })
}

fn optional_text(value: Option<JsonString<'_>>) -> Option<&str> {
    value.and_then(|value| value.as_str())
}

fn text(value: JsonString<'_>) -> io::Result<String> {
    if let Some(value) = value.as_str() {
        return Ok(value.to_owned());
    }
    let mut buffer = vec![0; value.encoded().len()];
    Ok(checked(value.decode_into(&mut buffer))?.to_owned())
}

fn checked<T, E: fmt::Display>(result: Result<T, E>) -> io::Result<T> {
    result.map_err(|error| invalid_data(error.to_string()))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn user_agent() -> &'static str {
    concat!("oci-zero/", env!("CARGO_PKG_VERSION"))
}

#[test]
#[ignore = "requires access to public OCI registries"]
fn inspects_public_registry_fixtures() -> Result<(), Box<dyn Error>> {
    for (index, reference) in SMOKE_REFERENCES.iter().enumerate() {
        if index != 0 {
            println!();
        }
        println!("==> {reference}");
        inspect(reference, *reference == REFERRERS_REFERENCE)?;
    }
    Ok(())
}
