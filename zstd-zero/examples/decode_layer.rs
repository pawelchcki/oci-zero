use std::error::Error;
use std::io::{self, Read};

use sha2::{Digest, Sha256};
use zstd_zero::{DecodeStep, Decoder, DecoderBuffers, MAX_BLOCK_SIZE};

const LAYER_URL: &str = "https://install.datadoghq.com/v2/agent-package/blobs/sha256:bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const COMPRESSED_SIZE: u64 = 3_651_442;
const COMPRESSED_SHA256: &str = "bc219703080f03ad836d51bf7f72cc3ced34f5ba440a49f450d6a5dea98ceff4";
const DECOMPRESSED_SIZE: u64 = 10_878_976;
const DECOMPRESSED_SHA256: &str =
    "5a1ed449ecc69d1accd5c5ddca23eda62f214bcfb5f2e953c6da38caf17d26ae";
const HISTORY_SIZE: usize = 32 * 1024 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let response = ureq::get(LAYER_URL)
        .set(
            "User-Agent",
            concat!("zstd-zero/", env!("CARGO_PKG_VERSION")),
        )
        .call()?;
    let mut reader = response.into_reader();
    let mut history = vec![0u8; HISTORY_SIZE];
    let mut block = vec![0u8; MAX_BLOCK_SIZE];
    let mut literals = vec![0u8; MAX_BLOCK_SIZE];
    let mut decoder = Decoder::new(DecoderBuffers {
        history: &mut history,
        block: &mut block,
        literals: &mut literals,
    });
    let mut compressed_hash = Sha256::new();
    let mut decompressed_hash = Sha256::new();
    let mut compressed_size = 0u64;
    let mut decompressed_size = 0u64;
    let mut input_buffer = [0u8; 16 * 1024];

    loop {
        let length = reader.read(&mut input_buffer)?;
        if length == 0 {
            break;
        }
        compressed_hash.update(&input_buffer[..length]);
        compressed_size += length as u64;
        drive(
            &mut decoder,
            &input_buffer[..length],
            &mut decompressed_hash,
            &mut decompressed_size,
        )?;
    }
    drain(&mut decoder, &mut decompressed_hash, &mut decompressed_size)?;
    decoder.finish().map_err(decoder_error)?;

    let compressed_digest = format!("{:x}", compressed_hash.finalize());
    let decompressed_digest = format!("{:x}", decompressed_hash.finalize());
    verify("compressed size", compressed_size, COMPRESSED_SIZE)?;
    verify("decompressed size", decompressed_size, DECOMPRESSED_SIZE)?;
    verify(
        "compressed digest",
        compressed_digest.as_str(),
        COMPRESSED_SHA256,
    )?;
    verify(
        "decompressed digest",
        decompressed_digest.as_str(),
        DECOMPRESSED_SHA256,
    )?;
    println!(
        "decoded {compressed_size} compressed bytes into {decompressed_size} bytes (sha256:{decompressed_digest})"
    );
    Ok(())
}

fn drive(
    decoder: &mut Decoder<'_>,
    mut input: &[u8],
    output_hash: &mut Sha256,
    output_size: &mut u64,
) -> Result<(), Box<dyn Error>> {
    while !input.is_empty() {
        let step = decoder.decode(input).map_err(decoder_error)?;
        let consumed = step.consumed();
        input = &input[consumed..];
        match step {
            DecodeStep::Output { bytes, .. } => {
                output_hash.update(bytes);
                *output_size += bytes.len() as u64;
            }
            DecodeStep::NeedInput { .. } if !input.is_empty() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "decoder requested input before consuming the available bytes",
                )
                .into());
            }
            _ => {}
        }
    }
    Ok(())
}

fn drain(
    decoder: &mut Decoder<'_>,
    output_hash: &mut Sha256,
    output_size: &mut u64,
) -> Result<(), Box<dyn Error>> {
    loop {
        match decoder.decode(&[]).map_err(decoder_error)? {
            DecodeStep::Output { bytes, .. } => {
                output_hash.update(bytes);
                *output_size += bytes.len() as u64;
            }
            DecodeStep::FrameStarted { .. } | DecodeStep::FrameFinished { .. } => {}
            DecodeStep::NeedInput { .. } => return Ok(()),
        }
    }
}

fn decoder_error(error: zstd_zero::DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
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
