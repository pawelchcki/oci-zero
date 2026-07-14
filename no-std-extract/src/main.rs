#![no_main]
#![no_std]

use core::cell::UnsafeCell;
use core::ffi::c_char;
use core::panic::PanicInfo;
use core::slice;

use embedded_io_async::Read;
use oci_zero::compression::zstd::{DecoderBuffers, MAX_BLOCK_SIZE};
use oci_zero::digest::Digest;
use oci_zero::layer::{
    Decoder, EntryLayerError, LayerError, VerifiedDecoder, VerifiedEntryExtractor,
};
use oci_zero::tar::ExtractError;
use rustix::fd::BorrowedFd;
use rustix::io::Errno;

mod platform;
mod source;
mod tls_heap;

// A hosted no_std executable still enters through the C runtime and uses its
// memory primitives and system-call wrappers. Rust's standard library would
// normally add this native dependency, so declare it explicitly here.
#[link(name = "c")]
extern "C" {}

const DEFAULT_ENTRY: &[u8] = b"application_monitoring.yaml.example";
const MAX_ENTRY_PATH: usize = 255;
const MAX_ARGUMENT: usize = 1024;
const HISTORY_CAPACITY: usize = 32 * 1024 * 1024;
const INPUT_SIZE: usize = 16 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct Fixture {
    compressed_digest: Digest,
    compressed_size: u64,
    decompressed_digest: Digest,
    decompressed_size: u64,
    history_size: usize,
}

const DEFAULT_FIXTURE: Fixture = Fixture {
    compressed_digest: Digest::from_bytes([
        0xbc, 0x21, 0x97, 0x03, 0x08, 0x0f, 0x03, 0xad, 0x83, 0x6d, 0x51, 0xbf, 0x7f, 0x72, 0xcc,
        0x3c, 0xed, 0x34, 0xf5, 0xba, 0x44, 0x0a, 0x49, 0xf4, 0x50, 0xd6, 0xa5, 0xde, 0xa9, 0x8c,
        0xef, 0xf4,
    ]),
    compressed_size: 3_651_442,
    decompressed_digest: Digest::from_bytes([
        0x5a, 0x1e, 0xd4, 0x49, 0xec, 0xc6, 0x9d, 0x1a, 0xcc, 0xd5, 0xc5, 0xdd, 0xca, 0x23, 0xed,
        0xa6, 0x2f, 0x21, 0x4b, 0xcf, 0xb5, 0xf2, 0xe9, 0x53, 0xc6, 0xda, 0x38, 0xca, 0xf1, 0x7d,
        0x26, 0xae,
    ]),
    decompressed_size: 10_878_976,
    history_size: HISTORY_CAPACITY,
};

struct WorkBuffers {
    history: [u8; HISTORY_CAPACITY],
    block: [u8; MAX_BLOCK_SIZE],
    literals: [u8; MAX_BLOCK_SIZE],
}

struct StaticBuffers(UnsafeCell<WorkBuffers>);

// The C runtime invokes `main` once, on one thread, so this storage has a
// single mutable borrower for the lifetime of the process.
unsafe impl Sync for StaticBuffers {}

static BUFFERS: StaticBuffers = StaticBuffers(UnsafeCell::new(WorkBuffers {
    history: [0; HISTORY_CAPACITY],
    block: [0; MAX_BLOCK_SIZE],
    literals: [0; MAX_BLOCK_SIZE],
}));

#[derive(Clone, Copy)]
pub(crate) enum Failure {
    Usage,
    Input,
    Decode,
    Archive,
    Output,
    Integrity,
    Tls,
    Http,
}

impl Failure {
    const fn status(self) -> i32 {
        match self {
            Self::Usage => 2,
            Self::Input => 3,
            Self::Decode => 4,
            Self::Archive => 5,
            Self::Output => 6,
            Self::Integrity => 7,
            Self::Tls => 9,
            Self::Http => 10,
        }
    }

    const fn message(self) -> &'static [u8] {
        match self {
            Self::Usage => {
                b"usage: oci-zero-no-std-extract SOURCE [ENTRY [COMPRESSED_DIGEST \
COMPRESSED_SIZE DECOMPRESSED_DIGEST DECOMPRESSED_SIZE HISTORY_SIZE]] [--metrics]\n"
            }
            Self::Input => b"failed to open or read input\n",
            Self::Decode => b"invalid or truncated zstd stream\n",
            Self::Archive => b"invalid tar stream or entry not found\n",
            Self::Output => b"failed to write extracted entry to stdout\n",
            Self::Integrity => b"layer size or digest mismatch\n",
            Self::Tls => b"TLS connection or certificate verification failed\n",
            Self::Http => b"HTTP request or response failed\n",
        }
    }
}

#[no_mangle]
extern "C" fn main(argc: i32, argv: *const *const c_char) -> i32 {
    let source = if argc > 1 {
        // SAFETY: A hosted C runtime supplies `argc` valid NUL-terminated
        // pointers in `argv` for the duration of `main`.
        match unsafe { argument(argv, 1) } {
            Some(argument) => argument,
            None => return report(Failure::Usage),
        }
    } else {
        b"-"
    };
    let target = if argc > 2 {
        match unsafe { argument(argv, 2) } {
            Some(argument) if argument.len() <= MAX_ENTRY_PATH => argument,
            _ => return report(Failure::Usage),
        }
    } else {
        DEFAULT_ENTRY
    };
    let (fixture, metrics) = match parse_fixture(argc, argv) {
        Ok(arguments) => arguments,
        Err(error) => return report(error),
    };

    match platform::block_on(run(source, target, fixture)) {
        Ok(()) => {
            #[cfg(feature = "bench-metrics")]
            if metrics {
                report_bench_metrics(fixture);
            }
            let _ = metrics;
            0
        }
        Err(error) => report(error),
    }
}

fn parse_fixture(argc: i32, argv: *const *const c_char) -> Result<(Fixture, bool), Failure> {
    if argc <= 3 {
        return Ok((DEFAULT_FIXTURE, false));
    }
    if argc == 4 && unsafe { argument(argv, 3) } == Some(b"--metrics") {
        return Ok((DEFAULT_FIXTURE, true));
    }
    if !matches!(argc, 8 | 9) {
        return Err(Failure::Usage);
    }
    let compressed_digest = parse_digest(unsafe { argument(argv, 3) })?;
    let compressed_size = parse_number(unsafe { argument(argv, 4) })?;
    let decompressed_digest = parse_digest(unsafe { argument(argv, 5) })?;
    let decompressed_size = parse_number(unsafe { argument(argv, 6) })?;
    let history_size =
        usize::try_from(parse_number(unsafe { argument(argv, 7) })?).map_err(|_| Failure::Usage)?;
    if history_size == 0 || history_size > HISTORY_CAPACITY {
        return Err(Failure::Usage);
    }
    let metrics = argc == 9 && unsafe { argument(argv, 8) } == Some(b"--metrics");
    if argc == 9 && !metrics {
        return Err(Failure::Usage);
    }
    Ok((
        Fixture {
            compressed_digest,
            compressed_size,
            decompressed_digest,
            decompressed_size,
            history_size,
        },
        metrics,
    ))
}

fn parse_digest(argument: Option<&[u8]>) -> Result<Digest, Failure> {
    let argument =
        core::str::from_utf8(argument.ok_or(Failure::Usage)?).map_err(|_| Failure::Usage)?;
    Digest::parse(argument).map_err(|_| Failure::Usage)
}

fn parse_number(argument: Option<&[u8]>) -> Result<u64, Failure> {
    core::str::from_utf8(argument.ok_or(Failure::Usage)?)
        .map_err(|_| Failure::Usage)?
        .parse()
        .map_err(|_| Failure::Usage)
}

#[cfg(feature = "bench-metrics")]
fn report_bench_metrics(fixture: Fixture) {
    // These values expose the fixed storage that makes the hosted no_std
    // executable independent of a native heap at runtime.
    write_metric(b"tls_arena_peak_bytes", tls_heap::used());
    write_metric(b"tls_arena_capacity_bytes", tls_heap::capacity());
    write_metric(
        b"decoder_static_buffer_bytes",
        core::mem::size_of::<WorkBuffers>(),
    );
    write_metric(b"decoder_history_bytes", fixture.history_size);
}

#[cfg(feature = "bench-metrics")]
fn write_metric(name: &[u8], value: usize) {
    // SAFETY: The process inherits stderr and does not close or replace it.
    let stderr = unsafe { rustix::stdio::stderr() };
    let _ = write_all(stderr, b"oci_zero_metric_");
    let _ = write_all(stderr, name);
    let _ = write_all(stderr, b"=");

    let mut digits = [0u8; 20];
    let mut start = digits.len();
    let mut remaining = value;
    loop {
        start -= 1;
        digits[start] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    let _ = write_all(stderr, &digits[start..]);
    let _ = write_all(stderr, b"\n");
}

async fn run(source: &[u8], target: &[u8], fixture: Fixture) -> Result<(), Failure> {
    if source == b"-" {
        let mut reader = platform::StdinReader::new();
        return extract_reader(&mut reader, target, fixture).await;
    }
    if source.starts_with(b"https://") {
        let url = core::str::from_utf8(source).map_err(|_| Failure::Usage)?;
        return source::extract_https(url, target, fixture).await;
    }

    let mut reader = platform::FileReader::open(source).map_err(|_| Failure::Input)?;
    extract_reader(&mut reader, target, fixture).await
}

pub(crate) async fn extract_reader<R: Read>(
    reader: &mut R,
    target: &[u8],
    fixture: Fixture,
) -> Result<(), Failure> {
    // SAFETY: `main` is called once and `extract` does not expose these
    // references beyond this invocation.
    let buffers = unsafe { &mut *BUFFERS.0.get() };
    let decoder = Decoder::zstd(DecoderBuffers {
        history: &mut buffers.history[..fixture.history_size],
        block: &mut buffers.block,
        literals: &mut buffers.literals,
    });
    let decoder = VerifiedDecoder::new(
        decoder,
        fixture.compressed_digest,
        fixture.compressed_size,
        fixture.decompressed_digest,
    );
    let mut extractor = VerifiedEntryExtractor::new(decoder, target);
    let mut input = [0u8; INPUT_SIZE];

    // SAFETY: This single-threaded process inherits valid stdout and never
    // closes or replaces it.
    let stdout = unsafe { rustix::stdio::stdout() };

    loop {
        let length = reader.read(&mut input).await.map_err(|_| Failure::Input)?;
        if length == 0 {
            break;
        }
        extractor
            .push(&input[..length], |bytes| write_all(stdout, bytes))
            .map_err(entry_error)?;
    }
    extractor
        .finish(|bytes| write_all(stdout, bytes))
        .map_err(entry_error)?;

    if extractor.decompressed_size() != fixture.decompressed_size {
        return Err(Failure::Integrity);
    }
    Ok(())
}

fn entry_error(error: EntryLayerError<()>) -> Failure {
    match error {
        EntryLayerError::Layer(LayerError::Format(_)) => Failure::Decode,
        EntryLayerError::Layer(LayerError::Integrity(_)) => Failure::Integrity,
        EntryLayerError::Layer(LayerError::Output(ExtractError::Output(()))) => Failure::Output,
        EntryLayerError::Layer(LayerError::Output(_)) | EntryLayerError::Finish(_) => {
            Failure::Archive
        }
    }
}

fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> Result<(), ()> {
    while !bytes.is_empty() {
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(()),
            Ok(written) => bytes = &bytes[written..],
            Err(Errno::INTR) => {}
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

fn report(error: Failure) -> i32 {
    // SAFETY: This single-threaded process inherits stderr and does not close
    // or replace it.
    let stderr = unsafe { rustix::stdio::stderr() };
    let _ = write_all(stderr, error.message());
    error.status()
}

unsafe fn argument<'a>(argv: *const *const c_char, index: usize) -> Option<&'a [u8]> {
    // SAFETY: The caller guarantees that `argv[index]` is a valid C string.
    let pointer = unsafe { *argv.add(index) }.cast::<u8>();
    if pointer.is_null() {
        return None;
    }
    for length in 0..=MAX_ARGUMENT {
        // SAFETY: Hosted argument strings are readable through their NUL byte.
        if unsafe { *pointer.add(length) } == 0 {
            // SAFETY: The scan above established that this many bytes are
            // readable, and the C runtime retains the storage through `main`.
            return Some(unsafe { slice::from_raw_parts(pointer, length) });
        }
    }
    None
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    extern "C" {
        fn _exit(status: i32) -> !;
    }

    // SAFETY: `_exit` terminates the current process without returning.
    unsafe { _exit(101) }
}

#[no_mangle]
extern "C" fn rust_eh_personality() {}
