//! Bake deterministic tiny OCI images into ROM using the crate's tar writer.
use std::{convert::Infallible, env, fs, path::PathBuf};

use oci_zero::{server::Content, tar::TarWriter};
use serde_json::{json, Value};

const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const LAYER: &str = "application/vnd.oci.image.layer.v1.tar";

fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = TarWriter::new();
    let mut result = Vec::new();
    let mut emit = |bytes: &[u8]| -> Result<(), Infallible> {
        result.extend_from_slice(bytes);
        Ok(())
    };
    for (name, bytes) in files {
        tar.begin_file(name.as_bytes(), bytes.len() as u64, 0o644, &mut emit)
            .unwrap();
        tar.write_file_data(bytes, &mut emit).unwrap();
        tar.end_file(&mut emit).unwrap();
    }
    tar.finish(&mut emit).unwrap();
    result
}

struct Builder {
    out: PathBuf,
    source: String,
}

impl Builder {
    fn content(&mut self, name: &str, bytes: &[u8], media_type: &str) -> Value {
        let content = Content::new(bytes, media_type);
        fs::write(self.out.join(name), bytes).unwrap();
        self.source.push_str(&format!(
            r#"static {name}: Content<'static> = Content {{
    bytes: include_bytes!(concat!(env!("OUT_DIR"), "/{name}")),
    media_type: {media_type:?},
    digest: Digest::from_bytes({:?}),
}};
"#,
            content.digest.as_bytes()
        ));
        json!({"mediaType": media_type, "digest": content.digest.to_string(), "size": bytes.len()})
    }

    fn image(&mut self, name: &str, architecture: &str, layers: &[Value]) -> Value {
        let config = serde_json::to_vec(&json!({
            "architecture": architecture, "os": "linux",
            "config": {},
            "rootfs": {"type": "layers", "diff_ids": layers.iter().map(|layer| layer["digest"].clone()).collect::<Vec<_>>()}
        })).unwrap();
        let config = self.content(&format!("{name}_CONFIG"), &config, CONFIG);
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2, "mediaType": MANIFEST, "config": config, "layers": layers
        }))
        .unwrap();
        self.content(name, &manifest, MANIFEST)
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../README.md");
    println!("cargo:rerun-if-changed=../../src/lib.rs");
    let mut builder = Builder {
        out: PathBuf::from(env::var_os("OUT_DIR").unwrap()),
        source: String::new(),
    };
    let base = archive(&[
        (
            "hello.txt",
            b"Hello from a registry living entirely in memory!\n",
        ),
        (
            "etc/motd",
            b"OCI Zero: tiny layers, zero storage services.\n",
        ),
        (
            "garden/seed.txt",
            b"This seed will be removed by the next layer.\n",
        ),
    ]);
    let base = builder.content("BASE", &base, LAYER);
    let garden = archive(&[
        ("garden/.wh.seed.txt", b""),
        (
            "garden/flower.txt",
            b"A flower grew in the overlay layer.\n",
        ),
        ("hello.txt", b"Hello from the second layer!\n"),
    ]);
    let garden = builder.content("GARDEN", &garden, LAYER);
    builder.image("V1", "amd64", std::slice::from_ref(&base));
    let v2 = builder.image("V2", "amd64", &[base.clone(), garden.clone()]);
    let arm = builder.image("ARM", "arm64", &[base, garden]);
    let mut amd_descriptor = v2.clone();
    amd_descriptor["platform"] = json!({"os": "linux", "architecture": "amd64"});
    let mut arm_descriptor = arm;
    arm_descriptor["platform"] = json!({"os": "linux", "architecture": "arm64"});
    builder.content(
        "INDEX",
        &serde_json::to_vec(&json!({
            "schemaVersion": 2, "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [amd_descriptor, arm_descriptor]
        }))
        .unwrap(),
        "application/vnd.oci.image.index.v1+json",
    );
    let readme = fs::read("../../README.md").unwrap();
    let lib = fs::read("../../src/lib.rs").unwrap();
    let source = archive(&[
        ("README.md", &readme),
        ("src/lib.rs", &lib),
        (
            "memfs/note.txt",
            b"Real repository files and a generated file share this layer.\n",
        ),
    ]);
    let source = builder.content("SOURCE_LAYER", &source, LAYER);
    let config = builder.content("SOURCE_CONFIG", b"{}", "application/vnd.oci.empty.v1+json");
    builder.content(
        "SOURCE",
        &serde_json::to_vec(&json!({
            "schemaVersion": 2, "mediaType": MANIFEST,
            "artifactType": "application/vnd.oci-zero.source.v1",
            "config": config, "layers": [source],
            "annotations": {"org.opencontainers.image.title": "A pocket copy of OCI Zero"}
        }))
        .unwrap(),
        MANIFEST,
    );
    fs::write(builder.out.join("content.rs"), builder.source).unwrap();
}
