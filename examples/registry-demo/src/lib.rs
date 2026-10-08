//! Shared ROM-backed registry used by both the TCP and Cloudflare demos.
#![no_std]
#![forbid(unsafe_code)]

// The hosted cdylib needs a panic runtime. Bare-metal builds use only the rlib
// and remain independent of std, just like the router and store themselves.
#[cfg(not(target_os = "none"))]
extern crate std;

use oci_zero::{
    digest::Digest,
    server::{Content, MemoryStore, Repository, Tag},
};

include!(concat!(env!("OUT_DIR"), "/content.rs"));

/// The demo's borrowed data also compiles for bare-metal targets.
pub static STORE: MemoryStore<'static> = MemoryStore(&[
    Repository {
        name: "demo/garden",
        tags: &[
            Tag {
                name: "latest",
                digest: V2.digest,
            },
            Tag {
                name: "v1",
                digest: V1.digest,
            },
            Tag {
                name: "v2",
                digest: V2.digest,
            },
            Tag {
                name: "multi",
                digest: INDEX.digest,
            },
        ],
        manifests: &[V1, V2, ARM, INDEX],
        blobs: &[V1_CONFIG, V2_CONFIG, ARM_CONFIG, BASE, GARDEN],
    },
    Repository {
        name: "demo/source",
        tags: &[Tag {
            name: "latest",
            digest: SOURCE.digest,
        }],
        manifests: &[SOURCE],
        blobs: &[SOURCE_CONFIG, SOURCE_LAYER],
    },
]);

#[cfg(feature = "wasm")]
mod wasm {
    extern crate alloc;
    use alloc::{
        string::{String, ToString},
        vec::Vec,
    };
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    pub struct RegistryResponse {
        status: u16,
        media_type: String,
        body: Vec<u8>,
        length: usize,
        digest: Option<String>,
        link: Option<String>,
    }

    #[wasm_bindgen]
    impl RegistryResponse {
        #[wasm_bindgen(getter)]
        pub fn status(&self) -> u16 {
            self.status
        }
        #[wasm_bindgen(getter)]
        pub fn media_type(&self) -> String {
            self.media_type.clone()
        }
        #[wasm_bindgen(getter)]
        pub fn body(&self) -> Vec<u8> {
            self.body.clone()
        }
        #[wasm_bindgen(getter)]
        pub fn length(&self) -> usize {
            self.length
        }
        #[wasm_bindgen(getter)]
        pub fn digest(&self) -> Option<String> {
            self.digest.clone()
        }
        #[wasm_bindgen(getter)]
        pub fn link(&self) -> Option<String> {
            self.link.clone()
        }
    }

    #[wasm_bindgen]
    pub fn dispatch(method: &str, target: &str) -> Result<RegistryResponse, JsValue> {
        let mut scratch = [0; 4096];
        let response = oci_zero::server::serve(&super::STORE, method, target, &mut scratch)
            .map_err(|_| JsValue::from_str("listing buffer too small"))?;
        Ok(RegistryResponse {
            status: response.status,
            media_type: response.media_type.into(),
            body: response.body.into(),
            length: response.content_length,
            digest: response.digest.map(|value| value.to_string()),
            link: response.next.map(|value| value.to_string()),
        })
    }
}
