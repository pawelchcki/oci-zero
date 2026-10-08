use oci_zero::{
    digest::Digest,
    server::{serve, BufferTooSmall, Content, MemoryStore, Repository, Store, Tag},
};

#[test]
fn serves_borrowed_content_by_tag_and_digest_with_head_metadata() {
    let manifest = Content::new(b"{}", "application/vnd.oci.image.manifest.v1+json");
    let blob = Content::new(b"hello", "text/plain");
    let tags = [Tag {
        name: "latest",
        digest: manifest.digest,
    }];
    let repositories = [Repository {
        name: "a/nested",
        tags: &tags,
        manifests: &[manifest],
        blobs: &[blob],
    }];
    let store = MemoryStore(&repositories);
    let mut scratch = [];
    let result = serve(&store, "GET", "/v2/a/nested/manifests/latest", &mut scratch).unwrap();
    assert_eq!(result.status, 200);
    assert_eq!(result.body.as_ptr(), manifest.bytes.as_ptr());
    assert_eq!(result.media_type, manifest.media_type);
    assert_eq!(result.digest, Some(manifest.digest));
    let target = format!("/v2/a/nested/manifests/{}", manifest.digest);
    let head = serve(&store, "HEAD", &target, &mut scratch).unwrap();
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.content_length, manifest.bytes.len());
    assert_eq!(head.digest, Some(manifest.digest));
    let target = format!("/v2/a/nested/blobs/{}", blob.digest);
    let result = serve(&store, "GET", &target, &mut scratch).unwrap();
    assert_eq!(result.body, b"hello");
    assert_eq!(result.media_type, "application/octet-stream");
}

#[test]
fn paginates_unsorted_repositories_and_tags_with_encoded_cursors() {
    let digest = Digest::from_bytes([0; 32]);
    let tags = [
        Tag { name: "v2", digest },
        Tag {
            name: "latest",
            digest,
        },
        Tag { name: "v1", digest },
    ];
    let store = MemoryStore(&[
        Repository {
            name: "z",
            tags: &[],
            manifests: &[],
            blobs: &[],
        },
        Repository {
            name: "a/nested",
            tags: &tags,
            manifests: &[],
            blobs: &[],
        },
    ]);
    let mut scratch = [0; 256];
    let result = serve(&store, "GET", "/v2/_catalog?n=1", &mut scratch).unwrap();
    assert_eq!(result.body, br#"{"repositories":["a/nested"]}"#);
    assert_eq!(
        result.next.unwrap().to_string(),
        "</v2/_catalog?n=1&last=a%2Fnested>; rel=\"next\""
    );
    let result = serve(
        &store,
        "GET",
        "/v2/_catalog?n=1&last=a%2Fnested",
        &mut scratch,
    )
    .unwrap();
    assert_eq!(result.body, br#"{"repositories":["z"]}"#);
    assert!(result.next.is_none());
    let result = serve(&store, "GET", "/v2/a/nested/tags/list?n=2", &mut scratch).unwrap();
    assert_eq!(
        result.body,
        br#"{"name":"a/nested","tags":["latest","v1"]}"#
    );
    assert_eq!(result.next.unwrap().last, "v1");
    let result = serve(
        &store,
        "GET",
        "/v2/a/nested/tags/list?n=2&last=v1",
        &mut scratch,
    )
    .unwrap();
    assert_eq!(result.body, br#"{"name":"a/nested","tags":["v2"]}"#);
    assert!(result.next.is_none());
}

#[test]
fn bounds_listing_output_and_rejects_malformed_pagination() {
    let store = MemoryStore(&[]);
    let mut scratch = [0; 128];
    assert!(matches!(
        serve(&store, "GET", "/v2/_catalog", &mut [0; 1]),
        Err(BufferTooSmall)
    ));
    for query in [
        "n=0",
        "n=-1",
        "n=+1",
        "n=wat",
        "n=1&n=2",
        "last=%",
        "last=%gg",
        "last=%ff",
        "last=x&last=y",
        "n",
    ] {
        let target = format!("/v2/_catalog?{query}");
        let result = serve(&store, "GET", &target, &mut scratch).unwrap();
        assert_eq!(result.status, 400, "{query}");
    }
    let target = format!("/v2/_catalog?last={}", "a".repeat(257));
    assert_eq!(
        serve(&store, "GET", &target, &mut [0; 128]).unwrap().status,
        400
    );
    let result = serve(&store, "GET", "/v2/_catalog?other=value", &mut scratch).unwrap();
    assert_eq!(result.body, br#"{"repositories":[]}"#);
}

#[test]
fn errors_are_oci_json_and_blobs_are_repository_scoped() {
    let blob = Content::new(b"hello", "text/plain");
    let store = MemoryStore(&[
        Repository {
            name: "a",
            tags: &[],
            manifests: &[],
            blobs: &[blob],
        },
        Repository {
            name: "b",
            tags: &[],
            manifests: &[],
            blobs: &[],
        },
    ]);
    let mut scratch = [];
    for (target, status, code) in [
        ("/v2/no/tags/list".into(), 404, "NAME_UNKNOWN"),
        ("/v2/a/manifests/missing".into(), 404, "MANIFEST_UNKNOWN"),
        (format!("/v2/b/blobs/{}", blob.digest), 404, "BLOB_UNKNOWN"),
        ("/v2/a/blobs/sha256:bad".into(), 400, "DIGEST_INVALID"),
    ] {
        let result = serve(&store, "GET", &target, &mut scratch).unwrap();
        assert_eq!(result.status, status);
        assert!(std::str::from_utf8(result.body).unwrap().contains(code));
        assert_eq!(result.media_type, "application/json");
        let length = result.content_length;
        let head = serve(&store, "HEAD", &target, &mut scratch).unwrap();
        assert!(head.body.is_empty());
        assert_eq!(head.content_length, length);
    }
    assert!(store.blob("b", blob.digest).is_none());
}

#[test]
fn supports_probe_and_preflight_and_rejects_writes() {
    let store = MemoryStore(&[]);
    let mut scratch = [];
    assert_eq!(
        serve(&store, "GET", "/v2/", &mut scratch).unwrap().body,
        b"{}"
    );
    let response = serve(&store, "OPTIONS", "/v2/any/path", &mut scratch).unwrap();
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty());
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        assert_eq!(
            serve(&store, method, "/v2/", &mut scratch).unwrap().status,
            405
        );
    }
}
