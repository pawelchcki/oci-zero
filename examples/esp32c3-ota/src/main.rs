//! ESP32-C3 firmware that pulls its own next version from an OCI registry.
//!
//! It has two modes, chosen at compile time:
//!
//! * **The device** (`--features matter`, or `full`). Commissions onto WiFi over
//!   BLE with the permanent QR code in the README, persists the credentials to the
//!   `nvs` partition, and stays on the fabric. BLE and WiFi run concurrently, so a
//!   commissioner can reach the node over IP without waiting for a reboot.
//! * **The measurement harness** (`--features measure`). Runs the `reference_*`
//!   functions and commissions nothing. Each one exists to drag a layer of the
//!   finished firmware into the linked image, because with `lto = "fat"` a
//!   dependency nothing calls is discarded entirely, so a rung that only added a
//!   `Cargo.toml` line would measure nothing. See MEASUREMENTS.md and measure.sh.
//!
//! The OCI self-update path is not implemented yet; only its cost is measured.
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

// At module scope because the `NODE` const below needs the `CLUSTER` associated
// items these traits provide.
#[cfg(all(feature = "matter", not(feature = "measure")))]
use rs_matter_embassy::matter::dm::clusters::app::on_off::OnOffHooks as _;
#[cfg(all(feature = "matter", not(feature = "measure")))]
use rs_matter_embassy::matter::dm::clusters::desc::ClusterHandler as _;

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

    info!("oci-zero esp32c3-ota {}", FIRMWARE_VERSION);
    info!("heap {} bytes, {} reclaimed", HEAP_SIZE, RECLAIMED_RAM);

    // One TRNG source for the whole program: `Trng::try_new()` only succeeds
    // while a `TrngSource` is alive, and both the TLS handshake and Matter's
    // crypto provider want one. Leaked rather than dropped, because dropping it
    // would turn the hardware RNG off again.
    #[cfg(any(feature = "tls", feature = "matter"))]
    core::mem::forget(esp_hal::rng::TrngSource::new(
        peripherals.RNG,
        peripherals.ADC1,
    ));

    #[cfg(feature = "measure")]
    {
        #[cfg(feature = "oci")]
        reference_oci_zero().await;

        #[cfg(feature = "tls")]
        reference_tls().await;

        #[cfg(feature = "radio")]
        reference_radio(peripherals.WIFI, peripherals.BT);

        #[cfg(feature = "matter")]
        reference_matter();

        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
        }
    }

    #[cfg(all(feature = "matter", not(feature = "measure")))]
    run_device(
        peripherals.FLASH,
        peripherals.WIFI,
        peripherals.BT,
        peripherals.GPIO9,
    )
    .await;

    #[cfg(not(any(feature = "matter", feature = "measure")))]
    {
        info!("built without `matter`: nothing to run. Try --features full.");
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
        }
    }
}

/// The version reported in Matter's Basic Information cluster and by
/// `esp_app_desc`.
///
/// CI sets `OCI_ZERO_FIRMWARE_VERSION` to the same string it writes into the OCI
/// artifact's `org.opencontainers.image.version` annotation, so a commissioner
/// shows the version that came out of the registry. A local build falls back to
/// the crate version.
const FIRMWARE_VERSION: &str = match option_env!("OCI_ZERO_FIRMWARE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Drives the real `pull()` walk over an in-memory registry, with the layer
/// inflated through `gzip-zero` and verified against its descriptor.
///
/// This is the code path Chunk 6 will keep; only the `Fetcher` changes, from
/// this canned one to mbedTLS + reqwless. Running it here is what puts the
/// manifest parser, the digest verifier and the gzip decoder in the image, and
/// what makes the `PullBuffers` sizes below a real RAM cost rather than a guess.
#[cfg(all(feature = "oci", feature = "measure"))]
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
#[cfg(all(feature = "oci", feature = "measure"))]
const MANIFEST: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.oci-zero.firmware.v1+json","config":{"mediaType":"application/vnd.oci-zero.firmware.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":108},"layers":[{"mediaType":"application/vnd.oci-zero.firmware.layer.v1.tar+gzip","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":1024}],"annotations":{"org.opencontainers.image.version":"0.0.0-measure","vnd.oci-zero.firmware.chip":"esp32c3"}}"#;

#[cfg(all(feature = "oci", feature = "measure"))]
const CONFIG: [u8; 108] = *br#"{"chip":"esp32c3","target":"riscv32imc-unknown-none-elf","entries":[{"path":"firmware.bin","offset":65536}]}"#;

#[cfg(all(feature = "oci", feature = "measure"))]
const LAYER: [u8; 1024] = [0; 1024];

/// Drives a real `embedded-tls` handshake, with certificate verification, over a
/// stream that fails on first read.
///
/// Unlike the other rungs this needs no trick to defeat dead-code elimination:
/// every input is constructible here, so the handshake is genuinely entered and
/// then fails at the first socket read. That links the record layer, the key
/// schedule, the X.509 chain verifier and the signature backends — which is the
/// whole cost of TLS on this part.
///
/// This is also where the C went. mbedTLS would have needed a cmake build, a
/// RISC-V-capable clang and four libc symbols this target does not have;
/// `embedded-tls` is pure Rust, so `cargo build` works on any host. `oci-zero`
/// needs no change either: its reqwless adapter takes any `Read + Write`, so a
/// `TlsConnection` plugs straight in.
#[cfg(all(feature = "tls", feature = "measure"))]
async fn reference_tls() {
    use embedded_io_async::{ErrorType, Read, Write};
    use embedded_tls::pki::CertVerifier;
    use embedded_tls::{
        Aes128GcmSha256, Certificate, CryptoProvider, NoClock, TlsConfig, TlsConnection,
        TlsContext, TlsVerifier,
    };

    /// Read buffer. TLS 1.3 permits a 16 KB plaintext record, and a client cannot
    /// ask a server for less — embedded-tls implements no max_fragment_length
    /// extension — so this has to hold a full record or a large response would
    /// fail mid-stream. It is the single largest RAM cost of the TLS layer.
    const READ_BUFFER: usize = 16 * 1024 + 256;
    /// Write buffer. Sized for what this firmware actually sends: a GET request
    /// with a bearer token, well under 2 KB. Nothing here uploads.
    const WRITE_BUFFER: usize = 2 * 1024 + 256;
    /// Scratch for one certificate while the chain is walked.
    const CERT_BUFFER: usize = 4096;

    #[derive(Debug)]
    struct Closed;

    impl core::fmt::Display for Closed {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str("the measurement stub has no socket")
        }
    }

    impl core::error::Error for Closed {}

    impl embedded_io_async::Error for Closed {
        fn kind(&self) -> embedded_io_async::ErrorKind {
            embedded_io_async::ErrorKind::ConnectionReset
        }
    }

    /// Stands in for the embassy-net socket Chunk 6 will supply. Writing
    /// succeeds so the ClientHello is actually built and encrypted; reading fails,
    /// so the handshake ends where the network would have begun.
    struct ClosedStream;

    impl ErrorType for ClosedStream {
        type Error = Closed;
    }

    impl Read for ClosedStream {
        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
            core::hint::black_box(&buffer);
            Err(Closed)
        }
    }

    impl Write for ClosedStream {
        async fn write(&mut self, bytes: &[u8]) -> Result<usize, Self::Error> {
            Ok(core::hint::black_box(bytes).len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Pairs the hardware TRNG with the X.509 chain verifier. esp-hal already
    /// implements the `rand_core` 0.6 traits embedded-tls wants for `Trng`, so no
    /// adapter is needed.
    struct VerifyingProvider {
        rng: esp_hal::rng::Trng,
        verifier: CertVerifier<'static, Aes128GcmSha256, NoClock, CERT_BUFFER>,
    }

    impl CryptoProvider for VerifyingProvider {
        type CipherSuite = Aes128GcmSha256;
        // Never used: no client certificate is offered, so nothing signs.
        type Signature = [u8; 64];

        fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
            &mut self.rng
        }

        fn verifier(
            &mut self,
        ) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, embedded_tls::TlsError> {
            Ok(&mut self.verifier)
        }
    }

    let Ok(trng) = esp_hal::rng::Trng::try_new() else {
        info!("tls: no TRNG available");
        return;
    };

    let mut read_record = [0u8; READ_BUFFER];
    let mut write_record = [0u8; WRITE_BUFFER];
    let config = TlsConfig::new()
        .with_server_name("ghcr.io")
        .enable_rsa_signatures();
    let mut connection = TlsConnection::new(ClosedStream, &mut read_record, &mut write_record);
    let provider = VerifyingProvider {
        rng: trng,
        verifier: CertVerifier::new(Certificate::X509(core::hint::black_box(CA_CERTIFICATE))),
    };

    let outcome = connection.open(TlsContext::new(&config, provider)).await;
    info!(
        "tls: handshake reached the socket ({:?}), buffers {} read + {} write",
        outcome.err(),
        READ_BUFFER,
        WRITE_BUFFER,
    );
}

/// Placeholder trust anchor. Sized like a real root so the verifier's buffers and
/// code paths are measured, but it is not a usable CA: Chunk 6 has to pin the
/// actual root for `ghcr.io` and the blob-redirect host, and supply a real clock
/// so validity dates are checked at all — `NoClock` skips them.
#[cfg(all(feature = "tls", feature = "measure"))]
const CA_CERTIFICATE: &[u8] = &[0; 1200];

/// Constructs the Wifi and BLE controllers, which is what pulls esp-radio's
/// driver code and PHY blobs into the image. Both are needed at once: BLE
/// carries commissioning and Wifi carries the OCI pull, and their coexistence is
/// the expensive case.
#[cfg(all(feature = "radio", feature = "measure"))]
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
#[cfg(all(feature = "matter", feature = "measure"))]
fn reference_matter() {
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

    info!(
        "matter stack: {} bytes static, bump {} bytes",
        core::mem::size_of::<EmbassyWifiMatterStack<BUMP_SIZE, ()>>(),
        BUMP_SIZE,
    );
    core::hint::black_box(stack);
}

/// Runs the Matter stack: BLE commissioning concurrently with WiFi, credentials
/// persisted to the `nvs` partition, and a BOOT-button factory reset.
///
/// `run_coex` rather than `run` is deliberate. Non-concurrent commissioning would
/// bring BLE down, join WiFi and only then be reachable, which needs a larger
/// `BUMP_SIZE` for the bigger futures and which some ecosystems handle badly.
/// Running both radios at once is the more expensive configuration, and it is the
/// one MEASUREMENTS.md measures.
#[cfg(all(feature = "matter", not(feature = "measure")))]
async fn run_device(
    flash: esp_hal::peripherals::FLASH<'static>,
    wifi: esp_hal::peripherals::WIFI<'static>,
    bt: esp_hal::peripherals::BT<'static>,
    boot_button: esp_hal::peripherals::GPIO9<'static>,
) -> ! {
    use core::pin::pin;

    use embassy_embedded_hal::adapter::BlockingAsync;
    use esp_bootloader_esp_idf::partitions::{
        read_partition_table, DataPartitionSubType, PartitionType, PARTITION_TABLE_MAX_LEN,
    };
    use esp_hal::gpio::{Input, InputConfig, Pull};
    use esp_storage::FlashStorage;
    use rs_matter_embassy::matter::crypto::{default_crypto, Crypto};
    use rs_matter_embassy::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic;
    use rs_matter_embassy::matter::dm::clusters::app::on_off::{self, OnOffHooks};
    use rs_matter_embassy::matter::dm::clusters::desc::{self, ClusterHandler as _};
    use rs_matter_embassy::matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM};
    use rs_matter_embassy::matter::dm::{Async, Dataver, EmptyHandler, EpClMatcher};
    use rs_matter_embassy::matter::utils::init::InitMaybeUninit;
    use rs_matter_embassy::matter::utils::select::Coalesce;
    use rs_matter_embassy::persist::SeqMapKvBlobStore;
    use rs_matter_embassy::stack::rand::reseeding_csprng;
    use rs_matter_embassy::wireless::esp::EspWifiDriver;
    use rs_matter_embassy::wireless::{EmbassyWifi, EmbassyWifiMatterStack};

    // Allocated statically: its footprint is 35-50 KB and putting that on the
    // program stack would blow it, and the wireless stack variation requires a
    // 'static stack anyway.
    let stack = mk_static!(EmbassyWifiMatterStack<BUMP_SIZE, ()>).init_with(
        EmbassyWifiMatterStack::init(&DEV_DET, TEST_DEV_COMM, &TEST_DEV_ATT),
    );

    // A reseeding CSPRNG over the hardware TRNG. `default_crypto` also wants the
    // device attestation private key, which is the test one — see the README on
    // why that means this is not a certified device.
    let crypto = default_crypto(
        reseeding_csprng(
            esp_hal::rng::Trng::try_new().expect("a TrngSource is alive"),
            1000,
        )
        .expect("the CSPRNG could not be seeded"),
        DAC_PRIVKEY,
    );
    let weak_rand = crypto.weak_rand().expect("a weak RNG");

    // Load any previously saved fabric state. Only a scratch buffer is needed to
    // parse the partition table, so it can be a local.
    let mut table_buffer = [0u8; PARTITION_TABLE_MAX_LEN];
    let mut flash = FlashStorage::new(flash);
    let table = read_partition_table(&mut flash, &mut table_buffer[..])
        .expect("the flash has no readable partition table");
    let nvs = table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .expect("the partition table could not be searched")
        .expect("the partition table has no nvs partition; see partitions.csv");
    let range = nvs.offset()..nvs.offset() + nvs.len();
    info!(
        "persisting Matter state to nvs partition {:?} at {:#x}..{:#x}",
        nvs.label_as_str(),
        range.start,
        range.end,
    );

    let mut store = SeqMapKvBlobStore::new(BlockingAsync::new(flash), range);
    stack
        .startup(&crypto, &mut store)
        .await
        .expect("the Matter stack could not start up");
    let kv = stack.matter().kv(store);

    if stack.is_commissioned() {
        info!("already commissioned; hold BOOT (GPIO9) for {RESET_SECS}s to wipe and start over");
    } else {
        info!("not commissioned: advertising over BLE");
        info!("  scan the QR code in README.md, or pair with code 34970112332");
    }

    // The handler chain for endpoint 1. `Dataver` needs randomness, which is why
    // this comes after the crypto provider.
    let mut dataver_rand = crypto.weak_rand().expect("a weak RNG");
    let on_off = on_off::OnOffHandler::new_standalone(
        Dataver::new_rand(&mut dataver_rand),
        UPDATER_ENDPOINT_ID,
        TestOnOffDeviceLogic::new(false),
    );
    let handler = EmptyHandler
        .chain(
            EpClMatcher::new(
                Some(UPDATER_ENDPOINT_ID),
                Some(TestOnOffDeviceLogic::CLUSTER.id),
            ),
            on_off::HandlerAsyncAdaptor(&on_off),
        )
        .chain(
            EpClMatcher::new(
                Some(UPDATER_ENDPOINT_ID),
                Some(desc::DescHandler::CLUSTER.id),
            ),
            Async(desc::DescHandler::new(Dataver::new_rand(&mut dataver_rand)).adapt()),
        );

    {
        // `pin!` is optional but shrinks the resulting future noticeably.
        let mut matter = pin!(stack.run_coex(
            EmbassyWifi::new(
                EspWifiDriver::new(wifi, bt),
                weak_rand,
                true, // a random BLE address, so the device is not trackable across boots
                stack,
            ),
            &crypto,
            (NODE, handler),
            &kv,
            (),
        ));

        // Whichever finishes first wins: Matter only returns on error, and the
        // reset watcher only returns when the button has been held long enough.
        let mut reset = pin!(wait_for_factory_reset(Input::new(
            boot_button,
            InputConfig::default().with_pull(Pull::Up),
        )));

        embassy_futures::select::select(&mut matter, &mut reset)
            .coalesce()
            .await
            .expect("the Matter stack stopped with an error");
    }

    log::warn!("wiping the persisted Matter state");
    stack
        .matter()
        .reset_persist(kv)
        .await
        .expect("the persisted state could not be wiped");

    log::warn!("rebooting");
    esp_hal::system::software_reset()
}

/// How long BOOT has to be held to wipe the fabric.
#[cfg(all(feature = "matter", not(feature = "measure")))]
const RESET_SECS: u64 = 3;

/// Resolves once BOOT has been held low for [`RESET_SECS`].
///
/// GPIO9 is the BOOT button on the usual ESP32-C3 boards, and it is pulled up, so
/// pressed means low. The debounce and the confirmation window exist because a
/// short press is how you enter the ROM bootloader — wiping a fabric on a stray
/// press would be a rude surprise.
#[cfg(all(feature = "matter", not(feature = "measure")))]
async fn wait_for_factory_reset(
    mut button: esp_hal::gpio::Input<'_>,
) -> Result<(), rs_matter_embassy::matter::error::Error> {
    loop {
        button.wait_for_low().await;
        embassy_time::Timer::after_millis(50).await;
        if !button.is_low() {
            continue;
        }

        log::warn!("BOOT held: keep it down for {RESET_SECS}s to wipe the Matter state");
        let outcome = embassy_futures::select::select(
            button.wait_for_high(),
            embassy_time::Timer::after_secs(RESET_SECS),
        )
        .await;

        if matches!(outcome, embassy_futures::select::Either::Second(())) {
            return Ok(());
        }
        log::info!("BOOT released early; not wiping");
    }
}

/// Basic Information for this device.
///
/// Vendor ID, product ID and serial number are `rs-matter`'s test values, because
/// the committed QR code is computed from exactly those — see
/// tools/matter-qr. The software version is *not* fixed: it carries
/// [`FIRMWARE_VERSION`], so a commissioner displays the version that came out of
/// the OCI registry.
#[cfg(all(feature = "matter", not(feature = "measure")))]
const DEV_DET: rs_matter_embassy::matter::dm::clusters::basic_info::BasicInfoConfig = {
    use rs_matter_embassy::matter::dm::devices::test::TEST_DEV_DET;

    rs_matter_embassy::matter::dm::clusters::basic_info::BasicInfoConfig {
        sw_ver_str: FIRMWARE_VERSION,
        product_name: "oci-zero OTA demo",
        device_name: "oci-zero",
        ..TEST_DEV_DET
    }
};

/// The endpoint the commissioner actually sees.
///
/// Endpoint 0 is the root, which carries Basic Information, Network Commissioning
/// and the rest of the system clusters. It is not enough on its own: a node whose
/// only endpoint is the root exposes no *device type*, and ecosystems will not
/// show a node they cannot put in a category — Google Home simply ignores it. The
/// first version of this firmware made that mistake.
#[cfg(all(feature = "matter", not(feature = "measure")))]
const UPDATER_ENDPOINT_ID: u16 = 1;

/// The Matter node.
///
/// The On/Off type is a stand-in, and knowingly so: Matter models no "thing that
/// updates itself", and picking a type that every ecosystem recognises is worth
/// more here than inventing an accurate one nothing can display. The switch state
/// carries no meaning — the point of the endpoint is to make the node visible so
/// it can be commissioned.
#[cfg(all(feature = "matter", not(feature = "measure")))]
const NODE: rs_matter_embassy::matter::dm::Node = rs_matter_embassy::matter::dm::Node {
    endpoints: &[
        rs_matter_embassy::wireless::EmbassyWifiMatterStack::<0, ()>::root_endpoint(),
        rs_matter_embassy::matter::dm::Endpoint::new(
            UPDATER_ENDPOINT_ID,
            rs_matter_embassy::matter::devices!(
                rs_matter_embassy::matter::dm::devices::DEV_TYPE_ON_OFF_LIGHT
            ),
            rs_matter_embassy::matter::clusters!(
                rs_matter_embassy::matter::dm::clusters::desc::DescHandler::CLUSTER,
                rs_matter_embassy::matter::dm::clusters::app::on_off::test::TestOnOffDeviceLogic::CLUSTER
            ),
        ),
    ],
};

/// Leaks a zeroed `'static` allocation for `$t`.
///
/// `rs-matter-embassy`'s examples define this; the Matter stack has to be
/// `'static` and must not live on the program stack.
#[cfg(all(feature = "matter", not(feature = "measure")))]
macro_rules! mk_static {
    ($t:ty) => {{
        static CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        CELL.uninit()
    }};
}
#[cfg(all(feature = "matter", not(feature = "measure")))]
use mk_static;
