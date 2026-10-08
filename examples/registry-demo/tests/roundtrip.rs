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
    digest::Verifier,
    layer::{Decoder, VerifiedDecoder, VerifiedEntryExtractor},
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
    manifests: usize,
    blobs: usize,
}
impl HttpFetcher<'_> {
    fn get(&self, request: Request<'_>) -> Result<ureq::Response, String> {
        let target = request.target;
        self.agent
            .get(&format!(
                "{}://{}{}",
                target.scheme, target.authority, target.path_and_query
            ))
            .set("Accept", request.accept)
            .call()
            .map_err(|e| e.to_string())
    }
}
impl Fetcher for HttpFetcher<'_> {
    type Error = String;

    async fn manifest(
        &mut self,
        reference: ManifestReference<'_>,
        destination: &mut [u8],
    ) -> Result<usize, String> {
        let mut path = [0; 512];
        let request = match reference {
            ManifestReference::Tag(_) => self.planner.manifest(&mut path),
            ManifestReference::Digest(digest) => self.planner.manifest_by_digest(digest, &mut path),
        }
        .map_err(|e| format!("{e:?}"))?;
        let response = self.get(request)?;
        let advertised = oci_zero::digest::Digest::parse(
            response
                .header("Docker-Content-Digest")
                .ok_or("missing digest")?,
        )
        .map_err(|e| format!("{e:?}"))?;
        let expected = match reference {
            ManifestReference::Digest(digest) => digest,
            _ => advertised,
        };
        if advertised != expected {
            return Err("manifest header mismatch".into());
        }
        let mut reader = response.into_reader();
        let mut length = 0;
        loop {
            let n = reader
                .read(&mut destination[length..])
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            length += n;
            if length == destination.len() {
                let mut excess = [0];
                if reader.read(&mut excess).map_err(|e| e.to_string())? != 0 {
                    return Err("manifest too large".into());
                }
                break;
            }
        }
        let mut verifier = Verifier::digest_only(expected);
        verifier
            .update(&destination[..length])
            .map_err(|e| format!("{e:?}"))?;
        verifier.finish().map_err(|e| format!("{e:?}"))?;
        self.manifests += 1;
        Ok(length)
    }

    async fn blob<S: BlobSink>(
        &mut self,
        descriptor: Descriptor<'_>,
        sink: &mut S,
    ) -> Result<(), String> {
        let digest = descriptor.digest();
        let mut path = [0; 512];
        let request = self
            .planner
            .blob(digest, "application/octet-stream", &mut path)
            .map_err(|e| format!("{e:?}"))?;
        let response = self.get(request)?;
        if response.header("Docker-Content-Digest") != Some(digest.to_string().as_str()) {
            return Err("blob header mismatch".into());
        }
        let mut verifier = Verifier::new(digest, descriptor.size());
        let mut reader = response.into_reader();
        // Force boundaries inside tar headers, file data, padding, and UTF-8.
        let mut chunk = [0; 37];
        let mut corrupted = false;
        let mut offset = 0;
        loop {
            let n = reader.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if self.corrupt_layer
                && !corrupted
                && offset >= 512
                && descriptor.media_type().as_str()
                    == Some("application/vnd.oci.image.layer.v1.tar")
            {
                chunk[0] ^= 1;
                corrupted = true;
            }
            verifier.update(&chunk[..n]).map_err(|e| format!("{e:?}"))?;
            offset += n;
            sink.chunk(&chunk[..n]);
            if sink.cancelled() {
                return Err("cancelled".into());
            }
        }
        verifier.finish().map_err(|e| format!("{e:?}"))?;
        self.blobs += 1;
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
    type Error = String;
    fn select_manifest(&mut self, descriptor: Descriptor<'_>) -> Result<Selection, String> {
        let platform = descriptor
            .platform()
            .map_err(|e| format!("{e:?}"))?
            .unwrap();
        Ok(if self.architecture == platform.architecture().as_str() {
            Selection::Pull
        } else {
            Selection::Skip
        })
    }
    fn manifest(&mut self, _: oci_zero::metadata::ImageManifest<'_>) -> Result<(), String> {
        self.manifests += 1;
        Ok(())
    }
    fn begin_blob(
        &mut self,
        kind: BlobKind,
        descriptor: Descriptor<'_>,
    ) -> Result<BlobAction, String> {
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
    fn blob_data(&mut self, kind: BlobKind, bytes: &[u8]) -> Result<(), String> {
        if matches!(kind, BlobKind::Layer { .. }) {
            self.extractor
                .as_mut()
                .unwrap()
                .push(bytes, |data| {
                    self.current.extend_from_slice(data);
                    Ok::<_, Infallible>(())
                })
                .map_err(|e| format!("{e:?}"))?;
        }
        Ok(())
    }
    fn end_blob(&mut self, kind: BlobKind) -> Result<(), String> {
        if matches!(kind, BlobKind::Layer { .. }) {
            let mut extractor = self.extractor.take().unwrap();
            let finished = extractor.finish(|data| {
                self.current.extend_from_slice(data);
                Ok::<_, Infallible>(())
            });
            match finished {
                Ok(())
                | Err(oci_zero::layer::EntryLayerError::Finish(
                    oci_zero::tar::FinishError::NotFound,
                )) => {}
                Err(error) => return Err(format!("{error:?}")),
            }
            if extractor.found() {
                self.files.push(self.current.clone());
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
) -> Result<Extract, String> {
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
        manifests: 0,
        blobs: 0,
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
    ))
    .map_err(|e| format!("{e:?}"))?;
    assert_eq!(
        fetcher.manifests,
        visitor.manifests + usize::from(architecture.is_some())
    );
    assert_eq!(fetcher.blobs, visitor.layers + visitor.manifests);
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
    assert!(error.contains("DigestMismatch"), "{error}");
}
#[test]
#[ignore = "requires a running Worker in OCI_ZERO_DEMO_REGISTRY_URL"]
fn rust_client_pulls_and_extracts_from_worker() {
    let origin = std::env::var("OCI_ZERO_DEMO_REGISTRY_URL").expect("Worker URL is required");
    roundtrip(origin.trim_end_matches('/'));
}
