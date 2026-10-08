use oci_zero::{
    metadata::{Document, MetadataError},
    registry::{basic_authorization, Credentials, RegistryError},
};

fn manifest(data: &str) -> String {
    format!(
        r#"{{"schemaVersion":2,"config":{{"mediaType":"x","size":0,"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","data":"{data}"}},"layers":[]}}"#
    )
}

#[test]
fn inline_data_requires_canonical_padded_base64() {
    for data in [
        "Zh==", "Zm9=", "=m9v", "Zg=", "Zg===", "Zg==AAAA", "_w==", "Z g==",
    ] {
        let json = manifest(data);
        let descriptor = Document::parse(json.as_bytes())
            .unwrap()
            .manifest()
            .unwrap()
            .config()
            .unwrap();
        assert_eq!(
            descriptor.decode_data(&mut [0; 32]),
            Err(MetadataError::InvalidBase64),
            "{data}"
        );
    }
    for (data, expected) in [
        ("", b"".as_slice()),
        ("Zg==", b"f"),
        ("Zm8=", b"fo"),
        ("Zm9v", b"foo"),
        ("Zm9vYg==", b"foob"),
        ("Zm9vYmE=", b"fooba"),
        ("Zm9vYmFy", b"foobar"),
        (r"Z\u0067==", b"f"),
    ] {
        let json = manifest(data);
        let descriptor = Document::parse(json.as_bytes())
            .unwrap()
            .manifest()
            .unwrap()
            .config()
            .unwrap();
        let mut buffer = [0; 32];
        assert_eq!(
            descriptor.decode_data(&mut buffer).unwrap(),
            Some(expected),
            "{data}"
        );
    }
}

#[test]
fn basic_authorization_handles_every_padding_length_and_exact_buffers() {
    for (username, password, expected) in [
        ("", "", "Basic Og=="),
        ("a", "", "Basic YTo="),
        ("a", "b", "Basic YTpi"),
        ("ab", "cd", "Basic YWI6Y2Q="),
        ("a", "b:c", "Basic YTpiOmM="),
    ] {
        let credentials = Credentials { username, password };
        let mut buffer = vec![0; expected.len()];
        assert_eq!(
            basic_authorization(credentials, &mut buffer).unwrap(),
            expected
        );
        assert_eq!(
            basic_authorization(credentials, &mut buffer[..expected.len() - 1]),
            Err(RegistryError::BufferTooSmall)
        );
    }
}

#[test]
fn deterministic_tar_headers_preserve_zero_fields_and_checksum() {
    let mut bytes = Vec::new();
    oci_zero::tar::TarWriter::new()
        .begin_file(b"example.txt", 5, 0o644, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(&bytes[108..116], b"0000000\0");
    assert_eq!(&bytes[116..124], b"0000000\0");
    assert_eq!(&bytes[136..148], b"00000000000\0");
    let checksum: u64 = bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            u64::from(if (148..156).contains(&index) {
                b' '
            } else {
                *byte
            })
        })
        .sum();
    assert_eq!(&bytes[148..156], format!("{checksum:06o}\0 ").as_bytes());
}
