//! Real HTTP transport exercising the crate's client against its memory server.
#![cfg(feature = "local")]

use std::{
    convert::Infallible,
    future::Future,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    task::{Context, Poll, Waker},
};

use oci_zero::{
    digest::{Digest, Verifier, VerifyError},
    layer::{Decoder, EntryLayerError, VerifiedDecoder, VerifiedEntryExtractor},
    metadata::Descriptor,
    pull::{
        self, BlobAction, BlobKind, BlobSink, Fetcher, ManifestReference, PullBuffers, PullVisitor,
        Selection,
    },
    reference::{Reference, Scheme},
    registry::{Request, RequestPlanner},
};

struct Server(Child);
impl Server {
    fn start() -> (Self, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_oci-zero-registry-demo"))
            .arg("127.0.0.1:0")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stderr.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let origin = line
            .trim()
            .strip_prefix("In-memory OCI demo: ")
            .unwrap()
            .to_owned();
        (Self(child), origin)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct HttpFetcher<'a> {
    planner: RequestPlanner<'a>,
    agent: ureq::Agent,
    corrupt_layer: bool,
}
impl HttpFetcher<'_> {
    fn get(&self, request: Request<'_>) -> ureq::Response {
        let target = request.target;
        self.agent
            .get(&format!(
                "{}://{}{}",
                target.scheme, target.authority, target.path_and_query
            ))
            .set("Accept", request.accept)
            .call()
            .unwrap()
    }
}
impl Fetcher for HttpFetcher<'_> {
    type Error = VerifyError;

    async fn manifest(
        &mut self,
        reference: ManifestReference<'_>,
        destination: &mut [u8],
    ) -> Result<usize, VerifyError> {
        let mut path = [0; 512];
        let request = match reference {
            ManifestReference::Tag(_) => self.planner.manifest(&mut path),
            ManifestReference::Digest(digest) => self.planner.manifest_by_digest(digest, &mut path),
        }
        .unwrap();
        let response = self.get(request);
        let advertised = Digest::parse(response.header("Docker-Content-Digest").unwrap()).unwrap();
        let expected = match reference {
            ManifestReference::Digest(digest) => digest,
            _ => advertised,
        };
        assert_eq!(advertised, expected, "manifest header mismatch");
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(destination.len() as u64 + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= destination.len(), "manifest too large");
        destination[..bytes.len()].copy_from_slice(&bytes);
        let mut verifier = Verifier::digest_only(expected);
        verifier.update(&bytes)?;
        verifier.finish()?;
        Ok(bytes.len())
    }

    async fn blob<S: BlobSink>(
        &mut self,
        descriptor: Descriptor<'_>,
        sink: &mut S,
    ) -> Result<(), VerifyError> {
        let digest = descriptor.digest();
        let mut path = [0; 512];
        let request = self
            .planner
            .blob(digest, "application/octet-stream", &mut path)
            .unwrap();
        let response = self.get(request);
        assert_eq!(
            response.header("Docker-Content-Digest"),
            Some(digest.to_string().as_str())
        );
        let mut verifier = Verifier::new(digest, descriptor.size());
        let mut reader = response.into_reader();
        // Force boundaries inside tar headers, file data, padding, and UTF-8.
        let mut chunk = [0; 37];
        let corrupt = self.corrupt_layer
            && descriptor.media_type().as_str() == Some("application/vnd.oci.image.layer.v1.tar");
        let mut offset = 0;
        loop {
            let n = reader.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            if corrupt && (offset..offset + n).contains(&512) {
                chunk[512 - offset] ^= 1;
            }
            verifier.update(&chunk[..n])?;
            offset += n;
            sink.chunk(&chunk[..n]);
            assert!(!sink.cancelled(), "unexpected visitor failure");
        }
        verifier.finish()?;
        Ok(())
    }
}

struct Extract {
    target: &'static [u8],
    architecture: Option<&'static str>,
    extractor: Option<VerifiedEntryExtractor<'static, 'static>>,
    current: Vec<u8>,
    files: Vec<Vec<u8>>,
    manifests: usize,
    layers: usize,
}
impl PullVisitor for Extract {
    type Error = EntryLayerError<Infallible>;
    fn select_manifest(&mut self, descriptor: Descriptor<'_>) -> Result<Selection, Self::Error> {
        let platform = descriptor.platform().unwrap().unwrap();
        Ok(if self.architecture == platform.architecture().as_str() {
            Selection::Pull
        } else {
            Selection::Skip
        })
    }
    fn manifest(&mut self, _: oci_zero::metadata::ImageManifest<'_>) -> Result<(), Self::Error> {
        self.manifests += 1;
        Ok(())
    }
    fn begin_blob(
        &mut self,
        kind: BlobKind,
        descriptor: Descriptor<'_>,
    ) -> Result<BlobAction, Self::Error> {
        if let BlobKind::Layer { diff_id } = kind {
            let digest = descriptor.digest();
            let size = descriptor.size();
            let decoder = match diff_id {
                Some(diff_id) => VerifiedDecoder::new(Decoder::Tar, digest, size, diff_id),
                None => VerifiedDecoder::compressed_only(Decoder::Tar, digest, size),
            };
            self.extractor = Some(VerifiedEntryExtractor::new(decoder, self.target));
            self.current.clear();
        }
        Ok(BlobAction::Fetch)
    }
    fn blob_data(&mut self, _: BlobKind, bytes: &[u8]) -> Result<(), Self::Error> {
        if let Some(extractor) = &mut self.extractor {
            extractor.push(bytes, |data| {
                self.current.extend_from_slice(data);
                Ok::<_, Infallible>(())
            })?;
        }
        Ok(())
    }
    fn end_blob(&mut self, _: BlobKind) -> Result<(), Self::Error> {
        if let Some(mut extractor) = self.extractor.take() {
            let finished = extractor.finish(|data| {
                self.current.extend_from_slice(data);
                Ok::<_, Infallible>(())
            });
            match finished {
                Ok(())
                | Err(oci_zero::layer::EntryLayerError::Finish(
                    oci_zero::tar::FinishError::NotFound,
                )) => {}
                Err(error) => return Err(error),
            }
            if extractor.found() {
                self.files.push(std::mem::take(&mut self.current));
            }
            self.layers += 1;
        }
        Ok(())
    }
}

fn ready<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    match std::pin::pin!(future)
        .as_mut()
        .poll(&mut Context::from_waker(waker))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("the blocking HTTP adapter unexpectedly yielded"),
    }
}

fn download(
    origin: &str,
    repository: &str,
    tag: &str,
    target: &'static [u8],
    architecture: Option<&'static str>,
    corrupt: bool,
) -> Result<Extract, pull::PullError<VerifyError, EntryLayerError<Infallible>>> {
    let authority = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap();
    let name = format!("oci://{authority}/{repository}:{tag}");
    let reference = Reference::parse(&name).unwrap();
    let mut fetcher = HttpFetcher {
        planner: RequestPlanner::with_scheme(
            reference,
            if origin.starts_with("https:") {
                Scheme::Https
            } else {
                Scheme::Http
            },
        ),
        agent: ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(10))
            .build(),
        corrupt_layer: corrupt,
    };
    let mut visitor = Extract {
        target,
        architecture,
        extractor: None,
        current: vec![],
        files: vec![],
        manifests: 0,
        layers: 0,
    };
    let mut root = [0; 4096];
    let mut child = [0; 4096];
    let mut config = [0; 4096];
    ready(pull::pull(
        &mut fetcher,
        reference,
        PullBuffers {
            root_manifest: &mut root,
            child_manifest: &mut child,
            config: &mut config,
        },
        &mut visitor,
    ))?;
    Ok(visitor)
}

fn roundtrip(origin: &str) {
    let image = download(origin, "demo/garden", "latest", b"hello.txt", None, false).unwrap();
    assert_eq!(image.layers, 2);
    assert_eq!(
        image.files,
        [
            b"Hello from a registry living entirely in memory!\n".to_vec(),
            b"Hello from the second layer!\n".to_vec()
        ]
    );
    for architecture in ["amd64", "arm64"] {
        let image = download(
            origin,
            "demo/garden",
            "multi",
            b"garden/flower.txt",
            Some(architecture),
            false,
        )
        .unwrap();
        assert_eq!(image.manifests, 1);
        assert_eq!(image.layers, 2);
        assert_eq!(
            image.files,
            [b"A flower grew in the overlay layer.\n".to_vec()]
        );
    }
    for (path, expected) in [
        (
            b"README.md".as_slice(),
            include_bytes!("../../../README.md").as_slice(),
        ),
        (
            b"src/lib.rs".as_slice(),
            include_bytes!("../../../src/lib.rs").as_slice(),
        ),
        (
            b"memfs/note.txt".as_slice(),
            b"Real repository files and a generated file share this layer.\n".as_slice(),
        ),
    ] {
        let artifact = download(origin, "demo/source", "latest", path, None, false).unwrap();
        assert_eq!(artifact.layers, 1);
        assert_eq!(artifact.files, [expected]);
    }
}

#[test]
fn rust_client_pulls_and_extracts_from_rust_server() {
    let (_server, origin) = Server::start();
    roundtrip(&origin);
}

#[test]
fn rust_client_rejects_corrupted_download() {
    let (_server, origin) = Server::start();
    let error = download(&origin, "demo/garden", "v1", b"hello.txt", None, true)
        .err()
        .unwrap();
    assert!(
        matches!(
            error,
            pull::PullError::Fetch(VerifyError::DigestMismatch { .. })
        ),
        "{error:?}"
    );
}
#[test]
#[ignore = "requires a running Worker in OCI_ZERO_DEMO_REGISTRY_URL"]
fn rust_client_pulls_and_extracts_from_worker() {
    let origin = std::env::var("OCI_ZERO_DEMO_REGISTRY_URL").expect("Worker URL is required");
    roundtrip(origin.trim_end_matches('/'));
}
