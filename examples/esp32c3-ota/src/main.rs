//! ESP32-C3 firmware that pulls its own next version from an OCI registry.
//!
//! Right now this binary exists to answer one question: does esp-hal +
//! esp-radio + `rs-matter-embassy` + `oci-zero` with TLS *fit* on this part?
//! See MEASUREMENTS.md for the answer and `measure.sh` for how it is taken.
//!
//! Each feature adds one layer of the eventual firmware. The layers are
//! *referenced*, not merely depended on: with `lto = "fat"` a dependency nothing
//! calls is discarded entirely, so a rung that only adds a `Cargo.toml` line
//! measures nothing. Every `reference_*` function below exists to drag the code
//! paths the finished firmware will use into the linked image, and each one ends
//! in a `black_box` so constant folding cannot delete the work.
//!
//! Nothing here commissions, connects or updates yet. Those arrive once the
//! measurement says they can.
#![no_std]
#![no_main]
#![recursion_limit = "256"]

use esp_backtrace as _;
use log::info;

extern crate alloc;

// rs-matter's C dependencies reference libc symbols this target lacks. mbedTLS
// needs some too, but gets them from the ROM libc esp-radio links; see the `tls`
// feature in Cargo.toml.
#[cfg(feature = "matter")]
use tinyrlibc as _;

esp_bootloader_esp_idf::esp_app_desc!();

/// Memory for the futures `rs-matter-stack` creates inside its `run*` methods,
/// served by a bump allocator rather than the heap. `rs-matter-embassy`'s own
/// example uses 20000 and notes that non-concurrent commissioning needs more.
#[cfg(feature = "matter")]
const BUMP_SIZE: usize = 20_000;

/// Heap for Wifi+BLE and for the one Matter dependency that needs `alloc`
/// (`x509`, ~4 KB). Must stay in step with `HEAP` in measure.sh.
const HEAP_SIZE: usize = 100 * 1024;

/// DRAM2 is only usable once the ROM code that lived there is finished with it.
const RECLAIMED_RAM: usize = esp_metadata_generated::memory_range!("DRAM2_UNINIT").end
    - esp_metadata_generated::memory_range!("DRAM2_UNINIT").start;

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) {
    esp_println::logger::init_logger_from_env();

    esp_alloc::heap_allocator!(size: HEAP_SIZE - RECLAIMED_RAM);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: RECLAIMED_RAM);

    let peripherals = esp_hal::init(esp_hal::Config::default());

    let timg0 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let software_interrupts =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupts.software_interrupt0);

    info!("oci-zero esp32c3-ota measurement build");
    info!("heap {} bytes, {} reclaimed", HEAP_SIZE, RECLAIMED_RAM);

    #[cfg(feature = "oci")]
    reference_oci_zero().await;

    #[cfg(feature = "tls")]
    reference_tls();

    #[cfg(feature = "radio")]
    reference_radio(peripherals.WIFI, peripherals.BT);

    #[cfg(feature = "matter")]
    reference_matter(peripherals.RNG, peripherals.ADC1);

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}

/// Drives the real `pull()` walk over an in-memory registry, with the layer
/// inflated through `gzip-zero` and verified against its descriptor.
///
/// This is the code path Chunk 6 will keep; only the `Fetcher` changes, from
/// this canned one to mbedTLS + reqwless. Running it here is what puts the
/// manifest parser, the digest verifier and the gzip decoder in the image, and
/// what makes the `PullBuffers` sizes below a real RAM cost rather than a guess.
#[cfg(feature = "oci")]
async fn reference_oci_zero() {
    use oci_zero::{
        digest::Digest,
        metadata::Descriptor,
        pull::{pull, BlobKind, BlobSink, Fetcher, ManifestReference, PullBuffers, PullVisitor},
        reference::Reference,
    };

    // Sized for a firmware artifact: one manifest, one small config, no index.
    // These are the numbers the measurement is really about.
    const MANIFEST_BUFFER: usize = 2048;
    const CONFIG_BUFFER: usize = 512;

    /// A canned registry. The bytes are the shape
    /// tools/build-firmware-artifact.sh emits.
    struct CannedFetcher;

    #[derive(Debug)]
    struct CannedError;

    impl Fetcher for CannedFetcher {
        type Error = CannedError;

        async fn manifest(
            &mut self,
            _reference: ManifestReference<'_>,
            destination: &mut [u8],
        ) -> Result<usize, Self::Error> {
            let manifest = core::hint::black_box(MANIFEST);
            if manifest.len() > destination.len() {
                return Err(CannedError);
            }
            destination[..manifest.len()].copy_from_slice(manifest);
            Ok(manifest.len())
        }

        async fn blob<S: BlobSink>(
            &mut self,
            descriptor: Descriptor<'_>,
            sink: &mut S,
        ) -> Result<(), Self::Error> {
            let blob = if descriptor.size() as usize == CONFIG.len() {
                core::hint::black_box(CONFIG.as_slice())
            } else {
                core::hint::black_box(LAYER.as_slice())
            };
            // Chunked, because the real transport delivers TLS records, and a
            // one-shot copy would not exercise the streaming decoder state.
            for chunk in blob.chunks(512) {
                if sink.cancelled() {
                    break;
                }
                sink.chunk(chunk);
            }
            Ok(())
        }
    }

    /// Counts what came back, so none of it can be optimised away.
    struct CountingVisitor {
        config_bytes: usize,
        layer_bytes: usize,
    }

    impl PullVisitor for CountingVisitor {
        type Error = CannedError;

        fn blob_data(&mut self, kind: BlobKind, bytes: &[u8]) -> Result<(), Self::Error> {
            match kind {
                BlobKind::Config => self.config_bytes += bytes.len(),
                BlobKind::Layer { .. } => self.layer_bytes += bytes.len(),
            }
            Ok(())
        }
    }

    let mut root_manifest = [0u8; MANIFEST_BUFFER];
    let mut child_manifest = [0u8; MANIFEST_BUFFER];
    let mut config = [0u8; CONFIG_BUFFER];
    let buffers = PullBuffers {
        root_manifest: &mut root_manifest,
        child_manifest: &mut child_manifest,
        config: &mut config,
    };

    let mut visitor = CountingVisitor {
        config_bytes: 0,
        layer_bytes: 0,
    };
    let reference = Reference::parse(core::hint::black_box(
        "oci://ghcr.io/pawelchcki/oci-zero-firmware:latest",
    ));

    let outcome = match reference {
        Ok(reference) => pull(&mut CannedFetcher, reference, buffers, &mut visitor)
            .await
            .is_ok(),
        Err(_) => false,
    };

    // The gzip decoder is what the firmware inflates the layer with, so give it
    // its real history window rather than letting LTO drop it.
    let mut history = [0u8; oci_zero::compression::gzip::HISTORY_SIZE];
    let decoder = oci_zero::layer::Decoder::gzip(oci_zero::compression::gzip::DecoderBuffers {
        history: &mut history,
    });
    let digest = Digest::parse(core::hint::black_box(
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    ));

    info!(
        "oci-zero: pull={} config={}B layer={}B gzip={} digest={}",
        outcome,
        visitor.config_bytes,
        visitor.layer_bytes,
        decoder.is_ok(),
        digest.is_ok(),
    );
    core::hint::black_box((&history, &visitor.config_bytes));
}

/// A manifest of the shape tools/build-firmware-artifact.sh writes. The digests
/// are wrong on purpose: `pull()` verifies them, so the walk fails at the blob
/// stage — after the parser, the descriptor handling and the digest code have
/// all been linked in, which is the only thing being measured.
#[cfg(feature = "oci")]
const MANIFEST: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.oci-zero.firmware.v1+json","config":{"mediaType":"application/vnd.oci-zero.firmware.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":108},"layers":[{"mediaType":"application/vnd.oci-zero.firmware.layer.v1.tar+gzip","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":1024}],"annotations":{"org.opencontainers.image.version":"0.0.0-measure","vnd.oci-zero.firmware.chip":"esp32c3"}}"#;

#[cfg(feature = "oci")]
const CONFIG: [u8; 108] = *br#"{"chip":"esp32c3","target":"riscv32imc-unknown-none-elf","entries":[{"path":"firmware.bin","offset":65536}]}"#;

#[cfg(feature = "oci")]
const LAYER: [u8; 1024] = [0; 1024];

/// Forces `oci_zero::tls::connect` to be monomorphised, which is what links
/// mbedTLS's handshake, X.509 and cipher code into the image.
///
/// The call is *linked but never executed*: `opaque_none` returns `None` behind
/// a `black_box`, so LTO cannot see that the branch is dead and must emit the
/// future's `poll`, while at runtime the branch is never taken. That avoids
/// needing a real socket, a real CA chain and the mbedTLS C allocator hooks —
/// all of which are Chunk 6's job — while still costing what Chunk 6 will cost.
///
/// The socket traits are implemented here rather than taken from embassy-net
/// because the generic parameters only affect the thin socket glue; the mbedTLS
/// code that dominates the measurement is identical either way.
#[cfg(feature = "tls")]
fn reference_tls() {
    use core::net::{IpAddr, SocketAddr};

    use embedded_io_async::{ErrorType, Read, Write};
    use embedded_nal_async::{AddrType, Dns, TcpConnect};

    #[derive(Debug)]
    struct Never;

    impl core::fmt::Display for Never {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("never")
        }
    }

    impl core::error::Error for Never {}

    impl embedded_io_async::Error for Never {
        fn kind(&self) -> embedded_io_async::ErrorKind {
            embedded_io_async::ErrorKind::Other
        }
    }

    struct NeverStream;

    impl ErrorType for NeverStream {
        type Error = Never;
    }

    impl Read for NeverStream {
        async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, Self::Error> {
            Err(Never)
        }
    }

    impl Write for NeverStream {
        async fn write(&mut self, _bytes: &[u8]) -> Result<usize, Self::Error> {
            Err(Never)
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Err(Never)
        }
    }

    struct NeverTcp;

    // DNS and connect must *succeed*, or LLVM proves everything past them
    // unreachable and folds the handshake away — leaving only mbedTLS's drop
    // glue in the image, which is exactly what this rung measured before.
    // Reading and writing may still fail: by then `Session::new` and
    // `session.connect()` have already been linked.
    impl TcpConnect for NeverTcp {
        type Error = Never;
        type Connection<'a> = NeverStream;

        async fn connect<'a>(
            &'a self,
            _remote: SocketAddr,
        ) -> Result<Self::Connection<'a>, Self::Error> {
            Ok(NeverStream)
        }
    }

    struct NeverDns;

    impl Dns for NeverDns {
        type Error = Never;

        async fn get_host_by_name(
            &self,
            _host: &str,
            _addr_type: AddrType,
        ) -> Result<IpAddr, Self::Error> {
            Ok(core::hint::black_box(IpAddr::V4(core::net::Ipv4Addr::new(
                127, 0, 0, 1,
            ))))
        }

        async fn get_host_by_address(
            &self,
            _addr: IpAddr,
            _result: &mut [u8],
        ) -> Result<usize, Self::Error> {
            Err(Never)
        }
    }

    // Opaque to the optimiser, always `None` at runtime.
    fn opaque_none<T>() -> Option<T> {
        core::hint::black_box(None)
    }

    let mut linked = false;
    if let Some((tls, ca_chain, server_name, target)) = opaque_none::<(
        mbedtls_rs::TlsReference<'static>,
        mbedtls_rs::Certificate<'static>,
        &'static core::ffi::CStr,
        oci_zero::registry::Target<'static>,
    )>() {
        // Polled, not merely constructed. Creating a future does not call its
        // `poll`, so LTO would drop the handshake body as unreachable and the
        // rung would measure nothing — which it did, until this line existed.
        let future =
            oci_zero::tls::connect(tls, &NeverTcp, &NeverDns, target, server_name, ca_chain);
        let mut future = core::pin::pin!(future);
        let mut context = core::task::Context::from_waker(core::task::Waker::noop());
        linked = core::future::Future::poll(future.as_mut(), &mut context).is_pending();
    }

    info!(
        "mbedTLS handshake path linked: {}",
        core::hint::black_box(linked)
    );
}

/// Constructs the Wifi and BLE controllers, which is what pulls esp-radio's
/// driver code and PHY blobs into the image. Both are needed at once: BLE
/// carries commissioning and Wifi carries the OCI pull, and their coexistence is
/// the expensive case.
#[cfg(feature = "radio")]
fn reference_radio(
    wifi: esp_hal::peripherals::WIFI<'static>,
    bt: esp_hal::peripherals::BT<'static>,
) {
    let wifi_controller =
        esp_radio::wifi::WifiController::new(wifi, esp_radio::wifi::ControllerConfig::default());
    let interface = esp_radio::wifi::Interface::station();
    let ble = esp_radio::ble::controller::BleConnector::new(bt, Default::default());

    info!(
        "esp-radio: wifi={} ble={} interface={:?}",
        wifi_controller.is_ok(),
        ble.is_ok(),
        interface,
    );
    // Leaked rather than dropped: teardown paths are not what is being measured,
    // and dropping would let LTO reason about the controllers' lifetimes.
    core::mem::forget(wifi_controller);
    core::mem::forget(ble);
}

/// Allocates the Matter stack exactly as `rs-matter-embassy`'s own example does,
/// so the measurement covers its real static footprint rather than a stub's.
#[cfg(feature = "matter")]
fn reference_matter(
    rng: esp_hal::peripherals::RNG<'static>,
    adc1: esp_hal::peripherals::ADC1<'static>,
) {
    use rs_matter_embassy::matter::dm::devices::test::{TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
    use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
    use rs_matter_embassy::wireless::EmbassyWifiMatterStack;

    static STACK: static_cell::StaticCell<EmbassyWifiMatterStack<BUMP_SIZE, ()>> =
        static_cell::StaticCell::new();

    let stack = STACK.uninit().init_with(EmbassyWifiMatterStack::init(
        &TEST_DEV_DET,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
    ));

    // The TRNG is the entropy source the crypto provider will reseed from.
    let trng = esp_hal::rng::TrngSource::new(rng, adc1);

    info!(
        "matter stack: {} bytes static, bump {} bytes",
        core::mem::size_of::<EmbassyWifiMatterStack<BUMP_SIZE, ()>>(),
        BUMP_SIZE,
    );
    core::hint::black_box(stack);
    core::mem::forget(trng);
}
