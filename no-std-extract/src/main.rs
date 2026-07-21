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

const DEFAULT_ENTRY: &[u8] = b"application_monitoring.yaml.example";
const MAX_ENTRY_PATH: usize = 255;
const MAX_ARGUMENT: usize = 1024;
const HISTORY_SIZE: usize = 32 * 1024 * 1024;
const INPUT_SIZE: usize = 16 * 1024;
const COMPRESSED_SIZE: u64 = 3_651_442;
const COMPRESSED_SHA256: [u8; 32] = [
    0xbc, 0x21, 0x97, 0x03, 0x08, 0x0f, 0x03, 0xad, 0x83, 0x6d, 0x51, 0xbf, 0x7f, 0x72, 0xcc, 0x3c,
    0xed, 0x34, 0xf5, 0xba, 0x44, 0x0a, 0x49, 0xf4, 0x50, 0xd6, 0xa5, 0xde, 0xa9, 0x8c, 0xef, 0xf4,
];
const DECOMPRESSED_SIZE: u64 = 10_878_976;
const DECOMPRESSED_SHA256: [u8; 32] = [
    0x5a, 0x1e, 0xd4, 0x49, 0xec, 0xc6, 0x9d, 0x1a, 0xcc, 0xd5, 0xc5, 0xdd, 0xca, 0x23, 0xed, 0xa6,
    0x2f, 0x21, 0x4b, 0xcf, 0xb5, 0xf2, 0xe9, 0x53, 0xc6, 0xda, 0x38, 0xca, 0xf1, 0x7d, 0x26, 0xae,
];

struct WorkBuffers {
    history: [u8; HISTORY_SIZE],
    block: [u8; MAX_BLOCK_SIZE],
    literals: [u8; MAX_BLOCK_SIZE],
}

struct StaticBuffers(UnsafeCell<WorkBuffers>);

// The C runtime invokes `main` once, on one thread, so this storage has a
// single mutable borrower for the lifetime of the process.
unsafe impl Sync for StaticBuffers {}

static BUFFERS: StaticBuffers = StaticBuffers(UnsafeCell::new(WorkBuffers {
    history: [0; HISTORY_SIZE],
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
            Self::Usage => b"usage: oci-zero-no-std-extract SOURCE [ENTRY]\n",
            Self::Input => b"failed to open or read input\n",
            Self::Decode => b"invalid or truncated zstd stream\n",
            Self::Archive => b"invalid tar stream or entry not found\n",
            Self::Output => b"failed to write extracted entry to stdout\n",
            Self::Integrity => b"Datadog layer size or digest mismatch\n",
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

    match platform::block_on(run(source, target)) {
        Ok(()) => 0,
        Err(error) => report(error),
    }
}

async fn run(source: &[u8], target: &[u8]) -> Result<(), Failure> {
    if source == b"-" {
        let mut reader = platform::StdinReader::new();
        return extract_reader(&mut reader, target).await;
    }
    if source.starts_with(b"https://") {
        let url = core::str::from_utf8(source).map_err(|_| Failure::Usage)?;
        return source::extract_https(url, target).await;
    }

    let mut reader = platform::FileReader::open(source).map_err(|_| Failure::Input)?;
    extract_reader(&mut reader, target).await
}

pub(crate) async fn extract_reader<R: Read>(reader: &mut R, target: &[u8]) -> Result<(), Failure> {
    // SAFETY: `main` is called once and `extract` does not expose these
    // references beyond this invocation.
    let buffers = unsafe { &mut *BUFFERS.0.get() };
    let decoder = Decoder::zstd(DecoderBuffers {
        history: &mut buffers.history,
        block: &mut buffers.block,
        literals: &mut buffers.literals,
    });
    let decoder = VerifiedDecoder::new(
        decoder,
        Digest::from_bytes(COMPRESSED_SHA256),
        COMPRESSED_SIZE,
        Digest::from_bytes(DECOMPRESSED_SHA256),
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

    if extractor.decompressed_size() != DECOMPRESSED_SIZE {
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
