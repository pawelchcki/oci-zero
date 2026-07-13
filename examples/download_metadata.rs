use std::collections::BTreeMap;
use std::env;
use std::error::Error;
use std::io::{self, Read};

use serde::de::DeserializeOwned;
use serde::Deserialize;

const DEFAULT_REFERENCE: &str = "oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9";
const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const MAX_METADATA_SIZE: u64 = 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageIndex {
    schema_version: u32,
    media_type: String,
    manifests: Vec<Descriptor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageManifest {
    schema_version: u32,
    media_type: String,
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
}

#[derive(Deserialize)]
struct Platform {
    architecture: String,
    os: String,
    #[serde(default)]
    variant: Option<String>,
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
}

fn invalid_reference(input: &str, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid OCI reference {input:?}: {reason}"),
    )
}

fn get_json<T: DeserializeOwned>(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
) -> Result<T, Box<dyn Error>> {
    let response = agent
        .get(url)
        .set("Accept", accept)
        .set(
            "User-Agent",
            concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
        )
        .call()?;

    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_METADATA_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("metadata response from {url} exceeds {MAX_METADATA_SIZE} bytes"),
        )
        .into());
    }

    Ok(serde_json::from_slice(&body)?)
}

fn platform_name(platform: Option<&Platform>) -> String {
    match platform {
        Some(platform) => match &platform.variant {
            Some(variant) => format!("{}/{}/{}", platform.os, platform.architecture, variant),
            None => format!("{}/{}", platform.os, platform.architecture),
        },
        None => "unknown platform".to_owned(),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let input = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_REFERENCE.to_owned());
    let reference = OciReference::parse(&input)?;
    let agent = ureq::AgentBuilder::new().build();
    let accept = format!("{INDEX_MEDIA_TYPE}, {MANIFEST_MEDIA_TYPE}");

    let index: ImageIndex = get_json(
        &agent,
        &reference.manifest_url(reference.reference),
        &accept,
    )?;
    println!(
        "{} (schema {}, {} platform manifests)",
        index.media_type,
        index.schema_version,
        index.manifests.len()
    );

    for descriptor in &index.manifests {
        let platform = platform_name(descriptor.platform.as_ref());
        println!(
            "\n{platform}: {} ({} bytes, {})",
            descriptor.digest, descriptor.size, descriptor.media_type
        );
        if let Some(artifact_type) = &descriptor.artifact_type {
            println!("  artifact type: {artifact_type}");
        }

        let manifest: ImageManifest = get_json(
            &agent,
            &reference.manifest_url(&descriptor.digest),
            MANIFEST_MEDIA_TYPE,
        )?;
        println!(
            "  manifest: schema {}, {}",
            manifest.schema_version, manifest.media_type
        );

        for key in [
            "com.datadoghq.package.name",
            "com.datadoghq.package.version",
            "com.datadoghq.package.size",
        ] {
            if let Some(value) = manifest.annotations.get(key) {
                println!("  {key}: {value}");
            }
        }

        let config: serde_json::Value = get_json(
            &agent,
            &reference.blob_url(&manifest.config.digest),
            &manifest.config.media_type,
        )?;
        println!(
            "  config: {} ({} bytes, {})",
            manifest.config.digest, manifest.config.size, manifest.config.media_type
        );
        println!("  config document: {config}");

        for layer in &manifest.layers {
            println!(
                "  skipped layer: {} ({} bytes, {})",
                layer.digest, layer.size, layer.media_type
            );
        }
    }

    Ok(())
}
