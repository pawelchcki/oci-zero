use std::env;
use std::error::Error;
use std::io::{self, Read, Write};

use oci_zero::compression::zstd::{DecoderBuffers, MAX_BLOCK_SIZE};
use oci_zero::digest::Digest;
use oci_zero::layer::{Decoder, LayerError, VerifiedDecoder};
use oci_zero::tar::{EntryExtractor, ExtractError, FinishError};

const LAYER_URL: &str = "https://install.datadoghq.com/v2/agent-package/blobs/sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const COMPRESSED_SIZE: u64 = 3_651_442;
const COMPRESSED_SHA256: &str =
    "sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const DECOMPRESSED_SIZE: u64 = 10_878_976;
const DECOMPRESSED_SHA256: &str =
    "sha256:5a1ed449ecc69d1accd5c5ddca23eda62f214bcfb5f2e953c6da38caf17d26ae";
const DEFAULT_ENTRY: &str = "application_monitoring.yaml.example";
const HISTORY_SIZE: usize = 32 * 1024 * 1024;

struct LayerOutput<'target, W> {
    extractor: EntryExtractor<'target>,
    writer: W,
    decompressed_size: u64,
    extracted_size: u64,
}

impl<W: Write> LayerOutput<'_, W> {
    fn consume(&mut self, bytes: &[u8]) -> Result<(), io::Error> {
        self.decompressed_size += bytes.len() as u64;
        self.extractor
            .push(bytes, |contents| {
                self.writer.write_all(contents)?;
                self.extracted_size += contents.len() as u64;
                Ok::<_, io::Error>(())
            })
            .map_err(extract_error)
    }
}

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
    let mut decoder = VerifiedDecoder::new(
        decoder,
        Digest::parse(COMPRESSED_SHA256).map_err(invalid_data)?,
        COMPRESSED_SIZE,
        Digest::parse(DECOMPRESSED_SHA256).map_err(invalid_data)?,
    );
    let mut layer_output = LayerOutput {
        extractor: EntryExtractor::new(target.as_bytes()),
        writer: io::stdout().lock(),
        decompressed_size: 0,
        extracted_size: 0,
    };
    let mut input_buffer = [0u8; 16 * 1024];

    loop {
        let length = reader.read(&mut input_buffer)?;
        if length == 0 {
            break;
        }
        decoder
            .push(&input_buffer[..length], |bytes| layer_output.consume(bytes))
            .map_err(layer_error)?;
    }
    decoder
        .finish(|bytes| layer_output.consume(bytes))
        .map_err(layer_error)?;
    layer_output.extractor.finish().map_err(finish_error)?;

    verify(
        "decompressed size",
        layer_output.decompressed_size,
        DECOMPRESSED_SIZE,
    )?;
    layer_output.writer.flush()?;
    eprintln!(
        "extracted {} bytes from {target:?}",
        layer_output.extracted_size
    );
    Ok(())
}

fn layer_error(error: LayerError<io::Error>) -> io::Error {
    match error {
        LayerError::Output(error) => error,
        error => invalid_data(error),
    }
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn extract_error(error: ExtractError<io::Error>) -> io::Error {
    match error {
        ExtractError::Output(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    }
}

fn finish_error(error: FinishError) -> io::Error {
    let kind = match error {
        FinishError::UnexpectedEof => io::ErrorKind::InvalidData,
        FinishError::NotFound => io::ErrorKind::NotFound,
    };
    io::Error::new(kind, error.to_string())
}

fn verify<T>(label: &str, actual: T, expected: T) -> Result<(), io::Error>
where
    T: Eq + std::fmt::Display,
{
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} mismatch: expected {expected}, got {actual}"),
        ))
    }
}
