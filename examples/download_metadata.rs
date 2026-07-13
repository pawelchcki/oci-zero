use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::io::{self, Read};

use serde::Deserialize;
use sha2::{Digest, Sha256};

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
const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const MANIFEST_ACCEPT: &str = concat!(
    "application/vnd.oci.image.index.v1+json, ",
    "application/vnd.oci.image.manifest.v1+json, ",
    "application/vnd.docker.distribution.manifest.list.v2+json, ",
    "application/vnd.docker.distribution.manifest.v2+json"
);
const MAX_METADATA_SIZE: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum ManifestDocument {
    Index(ImageIndex),
    Manifest(Box<ImageManifest>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageIndex {
    schema_version: u32,
    #[serde(default)]
    media_type: Option<String>,
    manifests: Vec<Descriptor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageManifest {
    schema_version: u32,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    artifact_type: Option<String>,
    #[serde(default)]
    subject: Option<Descriptor>,
    config: Descriptor,
    layers: Vec<Descriptor>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    media_type: String,
    size: u64,
    digest: String,
    #[serde(default)]
    platform: Option<Platform>,
    #[serde(default)]
    artifact_type: Option<String>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Platform {
    architecture: String,
    os: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

struct OciReference<'a> {
    registry: &'a str,
    repository: &'a str,
    reference: &'a str,
}

impl<'a> OciReference<'a> {
    fn parse(input: &'a str) -> Result<Self, io::Error> {
        let value = input
            .strip_prefix("oci://")
            .ok_or_else(|| invalid_reference(input, "expected an oci:// URL"))?;
        let (registry, repository_and_reference) = value
            .split_once('/')
            .ok_or_else(|| invalid_reference(input, "missing repository"))?;

        let (repository, reference) = match repository_and_reference.rsplit_once('@') {
            Some(parts) => parts,
            None => repository_and_reference
                .rsplit_once(':')
                .ok_or_else(|| invalid_reference(input, "missing tag or digest"))?,
        };

        if registry.is_empty() || repository.is_empty() || reference.is_empty() {
            return Err(invalid_reference(
                input,
                "registry, repository, or reference is empty",
            ));
        }

        Ok(Self {
            registry,
            repository,
            reference,
        })
    }

    fn manifest_url(&self, reference: &str) -> String {
        format!(
            "https://{}/v2/{}/manifests/{}",
            self.registry, self.repository, reference
        )
    }

    fn blob_url(&self, digest: &str) -> String {
        format!(
            "https://{}/v2/{}/blobs/{}",
            self.registry, self.repository, digest
        )
    }

    fn referrers_url(&self, digest: &str) -> String {
        format!(
            "https://{}/v2/{}/referrers/{}",
            self.registry, self.repository, digest
        )
    }
}

struct RegistryClient<'a> {
    reference: OciReference<'a>,
    agent: ureq::Agent,
    authorization: Option<String>,
}

impl<'a> RegistryClient<'a> {
    fn new(reference: OciReference<'a>) -> Self {
        Self {
            reference,
            agent: ureq::AgentBuilder::new().build(),
            authorization: None,
        }
    }

    fn manifest(&mut self, reference: &str) -> Result<FetchedBody, Box<dyn Error>> {
        self.get(&self.reference.manifest_url(reference), MANIFEST_ACCEPT)
    }

    fn blob(&mut self, descriptor: &Descriptor) -> Result<FetchedBody, Box<dyn Error>> {
        self.get(
            &self.reference.blob_url(&descriptor.digest),
            &descriptor.media_type,
        )
    }

    fn get(&mut self, url: &str, accept: &str) -> Result<FetchedBody, Box<dyn Error>> {
        let response = self
            .request(url, accept, false)?
            .ok_or_else(|| invalid_data("required registry object was not found"))?;
        read_response(response, url)
    }

    fn get_optional(
        &mut self,
        url: &str,
        accept: &str,
    ) -> Result<Option<FetchedBody>, Box<dyn Error>> {
        self.request(url, accept, true)?
            .map(|response| read_response(response, url))
            .transpose()
    }

    fn request(
        &mut self,
        url: &str,
        accept: &str,
        allow_not_found: bool,
    ) -> Result<Option<ureq::Response>, Box<dyn Error>> {
        for attempt in 0..2 {
            let mut request = self
                .agent
                .get(url)
                .set("Accept", accept)
                .set("User-Agent", user_agent());
            if let Some(authorization) = &self.authorization {
                request = request.set("Authorization", authorization);
            }

            match request.call() {
                Ok(response) => return Ok(Some(response)),
                Err(ureq::Error::Status(401, response)) if attempt == 0 => {
                    let challenge = response
                        .header("WWW-Authenticate")
                        .ok_or_else(|| invalid_data("registry returned 401 without a challenge"))?;
                    self.authorization = Some(fetch_bearer_authorization(&self.agent, challenge)?);
                }
                Err(ureq::Error::Status(404, _)) if allow_not_found => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        Err(invalid_data("registry rejected the Bearer token").into())
    }

    fn referrers(&mut self, digest: &str) -> Result<(FetchedBody, bool), Box<dyn Error>> {
        let url = self.reference.referrers_url(digest);
        if let Some(body) = self.get_optional(&url, INDEX_MEDIA_TYPE)? {
            return Ok((body, false));
        }

        let fallback_tag = digest.replacen(':', "-", 1);
        Ok((self.manifest(&fallback_tag)?, true))
    }
}

struct FetchedBody {
    bytes: Vec<u8>,
    content_type: Option<String>,
}

fn invalid_reference(input: &str, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid OCI reference {input:?}: {reason}"),
    )
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn user_agent() -> &'static str {
    concat!("oci-zero/", env!("CARGO_PKG_VERSION"))
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
    challenge: &str,
) -> Result<String, Box<dyn Error>> {
    let (scheme, parameters) = challenge
        .split_once(' ')
        .ok_or_else(|| invalid_data("malformed registry authentication challenge"))?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(invalid_data(format!(
            "unsupported registry authentication scheme {scheme:?}"
        ))
        .into());
    }

    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for parameter in parameters.split(',') {
        let (name, value) = parameter
            .trim()
            .split_once('=')
            .ok_or_else(|| invalid_data("malformed Bearer challenge parameter"))?;
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(|| invalid_data("Bearer challenge values must be quoted"))?;
        match name {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            "scope" => scope = Some(value),
            _ => {}
        }
    }

    let realm = realm.ok_or_else(|| invalid_data("Bearer challenge is missing its realm"))?;
    let mut request = agent
        .get(realm)
        .set("Accept", "application/json")
        .set("User-Agent", user_agent());
    if let Some(service) = service {
        request = request.query("service", service);
    }
    if let Some(scope) = scope {
        request = request.query("scope", scope);
    }

    let body = read_response(request.call()?, realm)?;
    let response: TokenResponse = serde_json::from_slice(&body.bytes)?;
    let token = response
        .token
        .or(response.access_token)
        .ok_or_else(|| invalid_data("registry token response did not contain a token"))?;
    Ok(format!("Bearer {token}"))
}

fn verify_digest(bytes: &[u8], expected: &str) -> Result<(), io::Error> {
    if !expected.starts_with("sha256:") {
        return Err(invalid_data(format!(
            "unsupported content digest {expected:?}"
        )));
    }
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "digest mismatch: expected {expected}, got {actual}"
        )))
    }
}

fn verify_descriptor(bytes: &[u8], descriptor: &Descriptor) -> Result<(), io::Error> {
    if bytes.len() as u64 != descriptor.size {
        return Err(invalid_data(format!(
            "size mismatch for {}: expected {}, got {}",
            descriptor.digest,
            descriptor.size,
            bytes.len()
        )));
    }
    verify_digest(bytes, &descriptor.digest)
}

fn platform_name(platform: Option<&Platform>) -> String {
    match platform {
        Some(platform) => match &platform.variant {
            Some(variant) => format!("{}/{}/{}", platform.os, platform.architecture, variant),
            None => format!("{}/{}", platform.os, platform.architecture),
        },
        None => "artifact".to_owned(),
    }
}

fn inspect_manifest(
    client: &mut RegistryClient<'_>,
    manifest: ImageManifest,
    response_media_type: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let media_type = manifest
        .media_type
        .as_deref()
        .or(response_media_type)
        .unwrap_or(MANIFEST_MEDIA_TYPE);
    println!(
        "  manifest: schema {}, {media_type}",
        manifest.schema_version
    );
    if manifest.schema_version != 2 {
        return Err(invalid_data(format!(
            "unsupported manifest schema {}",
            manifest.schema_version
        ))
        .into());
    }
    if let Some(artifact_type) = &manifest.artifact_type {
        println!("  artifact type: {artifact_type}");
    }
    if let Some(subject) = &manifest.subject {
        println!(
            "  subject: {} ({} bytes, {})",
            subject.digest, subject.size, subject.media_type
        );
        let subject_body = client.manifest(&subject.digest)?;
        verify_descriptor(&subject_body.bytes, subject)?;
    }

    for key in [
        "org.opencontainers.image.title",
        "org.opencontainers.image.version",
        "org.opencontainers.image.description",
        "com.datadoghq.package.name",
        "com.datadoghq.package.version",
        "com.datadoghq.package.size",
    ] {
        if let Some(value) = manifest.annotations.get(key) {
            println!("  {key}: {value}");
        }
    }

    let config = client.blob(&manifest.config)?;
    verify_descriptor(&config.bytes, &manifest.config)?;
    println!(
        "  config: {} ({} bytes, {})",
        manifest.config.digest, manifest.config.size, manifest.config.media_type
    );
    if let Ok(document) = serde_json::from_slice::<serde_json::Value>(&config.bytes) {
        println!("  config document: {document}");
    }

    for layer in &manifest.layers {
        let title = layer
            .annotations
            .get("org.opencontainers.image.title")
            .map(|title| format!(", {title}"))
            .unwrap_or_default();
        println!(
            "  skipped layer: {} ({} bytes, {}{title})",
            layer.digest, layer.size, layer.media_type
        );
    }
    Ok(())
}

fn inspect_referrers(
    client: &mut RegistryClient<'_>,
    subject_digest: &str,
) -> Result<(), Box<dyn Error>> {
    let (body, used_fallback) = client.referrers(subject_digest)?;
    let index: ImageIndex = serde_json::from_slice(&body.bytes)?;
    let source = if used_fallback {
        "referrers tag fallback"
    } else {
        "Referrers API"
    };
    let media_type = index
        .media_type
        .as_deref()
        .or(body.content_type.as_deref())
        .unwrap_or(INDEX_MEDIA_TYPE);
    println!(
        "\n{source}: {} referrers for {subject_digest} ({media_type})",
        index.manifests.len()
    );
    if index.schema_version != 2 || media_type != INDEX_MEDIA_TYPE {
        return Err(invalid_data(format!(
            "unsupported referrers index: schema {}, media type {media_type}",
            index.schema_version,
        ))
        .into());
    }
    if index.manifests.is_empty() {
        return Err(invalid_data("expected at least one public referrer").into());
    }

    for descriptor in &index.manifests {
        let child = client.manifest(&descriptor.digest)?;
        verify_descriptor(&child.bytes, descriptor)?;
        let ManifestDocument::Manifest(manifest) =
            serde_json::from_slice::<ManifestDocument>(&child.bytes)?
        else {
            return Err(invalid_data("referrer descriptor resolved to an OCI index").into());
        };

        let subject = manifest
            .subject
            .as_ref()
            .ok_or_else(|| invalid_data("referrer manifest is missing its subject"))?;
        if subject.digest != subject_digest {
            return Err(invalid_data(format!(
                "referrer subject mismatch: expected {subject_digest}, got {}",
                subject.digest
            ))
            .into());
        }
        let manifest_artifact_type = manifest
            .artifact_type
            .as_deref()
            .unwrap_or(&manifest.config.media_type);
        if descriptor.artifact_type.as_deref() != Some(manifest_artifact_type) {
            println!(
                "  fallback descriptor advertised {}, manifest declares {manifest_artifact_type}",
                descriptor
                    .artifact_type
                    .as_deref()
                    .unwrap_or("no artifact type")
            );
        }

        println!(
            "\nreferrer: {} ({} bytes, {})",
            descriptor.digest, descriptor.size, manifest_artifact_type
        );
        inspect_manifest(client, *manifest, child.content_type.as_deref())?;
    }
    Ok(())
}

fn inspect(input: &str, discover_referrers: bool) -> Result<(), Box<dyn Error>> {
    let reference = OciReference::parse(input)?;
    let root_reference = reference.reference.to_owned();
    let mut client = RegistryClient::new(reference);
    let root = client.manifest(&root_reference)?;
    if root_reference.starts_with("sha256:") {
        verify_digest(&root.bytes, &root_reference)?;
    }

    match serde_json::from_slice::<ManifestDocument>(&root.bytes)? {
        ManifestDocument::Index(index) => {
            let media_type = index
                .media_type
                .as_deref()
                .or(root.content_type.as_deref())
                .unwrap_or(INDEX_MEDIA_TYPE);
            println!(
                "{media_type} (schema {}, {} manifests)",
                index.schema_version,
                index.manifests.len()
            );
            if index.schema_version != 2 {
                return Err(invalid_data(format!(
                    "unsupported index schema {}",
                    index.schema_version
                ))
                .into());
            }

            for descriptor in &index.manifests {
                let platform = platform_name(descriptor.platform.as_ref());
                println!(
                    "\n{platform}: {} ({} bytes, {})",
                    descriptor.digest, descriptor.size, descriptor.media_type
                );
                if let Some(artifact_type) = &descriptor.artifact_type {
                    println!("  artifact type: {artifact_type}");
                }

                let child = client.manifest(&descriptor.digest)?;
                verify_descriptor(&child.bytes, descriptor)?;
                match serde_json::from_slice::<ManifestDocument>(&child.bytes)? {
                    ManifestDocument::Manifest(manifest) => {
                        inspect_manifest(&mut client, *manifest, child.content_type.as_deref())?;
                    }
                    ManifestDocument::Index(_) => {
                        return Err(invalid_data("nested OCI indexes are not supported").into());
                    }
                }
            }
        }
        ManifestDocument::Manifest(manifest) => {
            inspect_manifest(&mut client, *manifest, root.content_type.as_deref())?;
        }
    }
    if discover_referrers {
        inspect_referrers(&mut client, &root_reference)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let smoke = matches!(arguments.as_slice(), [argument] if argument == "--smoke");
    let references: Vec<&str> = match arguments.as_slice() {
        [] => vec![DEFAULT_REFERENCE],
        [argument] if argument == "--smoke" => SMOKE_REFERENCES.to_vec(),
        _ => arguments.iter().map(String::as_str).collect(),
    };

    for (index, reference) in references.iter().enumerate() {
        if index != 0 {
            println!();
        }
        println!("==> {reference}");
        inspect(reference, smoke && *reference == REFERRERS_REFERENCE)?;
    }
    Ok(())
}
