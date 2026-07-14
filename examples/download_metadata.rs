use std::error::Error;
use std::fmt;
use std::io::{self, Read};

use oci_zero::digest::Verifier;
use oci_zero::metadata::{DescriptorIter, Document, DocumentKind};
use oci_zero::reference::{Reference, Selector};
use oci_zero::registry::RequestPlanner;

const REFERENCE: &str = "oci://install.datadoghq.com/agent-package@sha256:7ab3a71476f068c21399250e66a2b1ab366437489510ee12c2119bba75afcde9";
const MAX_METADATA_SIZE: u64 = 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let reference = checked(Reference::parse(REFERENCE))?;
    let Selector::Digest(digest) = reference.selector() else {
        return Err(invalid_data("example reference must be digest-pinned").into());
    };

    let planner = RequestPlanner::new(reference);
    let mut path = [0; 512];
    let request = checked(planner.manifest(&mut path))?;
    let url = format!(
        "{}://{}{}",
        request.target.scheme, request.target.authority, request.target.path_and_query
    );
    let response = ureq::get(&url)
        .set("Accept", request.accept)
        .set(
            "User-Agent",
            concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
        )
        .call()?;
    let content_type = response.header("Content-Type").map(str::to_owned);
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_METADATA_SIZE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_METADATA_SIZE {
        return Err(invalid_data("manifest exceeds the example's 1 MiB limit").into());
    }

    let mut verifier = Verifier::digest_only(digest);
    checked(verifier.update(&bytes))?;
    checked(verifier.finish())?;

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
    println!(
        "downloaded and verified {kind} {digest} ({} bytes, {} entries, {})",
        bytes.len(),
        children,
        content_type.as_deref().unwrap_or("unknown media type")
    );
    Ok(())
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
