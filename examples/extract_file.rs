use std::env;
use std::error::Error;
use std::io::{self, Read, Write};

use oci_zero::compression::zstd::{DecoderBuffers, MAX_BLOCK_SIZE};
use oci_zero::digest::Digest;
use oci_zero::layer::{
    Decoder, EntryLayerError, LayerError, VerifiedDecoder, VerifiedEntryExtractor,
};
use oci_zero::tar::ExtractError;

const LAYER_URL: &str = "https://install.datadoghq.com/v2/agent-package/blobs/sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const COMPRESSED_SIZE: u64 = 3_651_442;
const COMPRESSED_SHA256: &str =
    "sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const DECOMPRESSED_SHA256: &str =
    "sha256:5a1ed449ecc69d1accd5c5ddca23eda62f214bcfb5f2e953c6da38caf17d26ae";
const DEFAULT_ENTRY: &str = "application_monitoring.yaml.example";
const HISTORY_SIZE: usize = 32 * 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let target = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_ENTRY.to_owned());
    let response = ureq::get(LAYER_URL)
        .set(
            "User-Agent",
            concat!("oci-zero/", env!("CARGO_PKG_VERSION")),
        )
        .call()?;
    let mut reader = response.into_reader();

    // These are the only large layer-processing allocations. The frame itself
    // declares a 32 MiB history window; the other two buffers are reusable
    // 128 KiB decoder work areas.
    let mut history = vec![0u8; HISTORY_SIZE];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let decoder = Decoder::zstd(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let decoder = VerifiedDecoder::new(
        decoder,
        Digest::parse(COMPRESSED_SHA256).map_err(invalid_data)?,
        COMPRESSED_SIZE,
        Digest::parse(DECOMPRESSED_SHA256).map_err(invalid_data)?,
    );
    let mut extractor = VerifiedEntryExtractor::new(decoder, target.as_bytes());
    let mut writer = io::stdout().lock();
    let mut input_buffer = [0u8; 16 * 1024];

    loop {
        let length = reader.read(&mut input_buffer)?;
        if length == 0 {
            break;
        }
        extractor
            .push(&input_buffer[..length], |bytes| writer.write_all(bytes))
            .map_err(entry_error)?;
    }
    extractor
        .finish(|bytes| writer.write_all(bytes))
        .map_err(entry_error)?;

    writer.flush()?;
    eprintln!(
        "extracted {} bytes from {target:?}",
        extractor.extracted_size()
    );
    Ok(())
}

fn entry_error(error: EntryLayerError<io::Error>) -> io::Error {
    match error {
        EntryLayerError::Layer(LayerError::Output(ExtractError::Output(error))) => error,
        error => invalid_data(error),
    }
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
