//! Generates the Matter onboarding payload for `examples/esp32c3-ota`.
//!
//! The payload is a pure function of vendor ID, product ID, discriminator and
//! passcode, so for a device with fixed commissioning data it is a build-time
//! constant — which is what makes committing a QR code to a README meaningful.
//!
//! Those four values are read from `rs-matter`'s own `TEST_DEV_DET` and
//! `TEST_DEV_COMM`, the very constants the firmware passes to
//! `EmbassyWifiMatterStack::init`. Nothing is duplicated, so the committed code
//! cannot describe a device other than the one that gets built. CI regenerates
//! and diffs the output, so changing the commissioning data fails the build until
//! the README is regenerated too.
use rs_matter::dm::devices::test::{TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::pairing::qr::{no_optional_data, CommFlowType, Qr, QrPayload, QrTextType};
use rs_matter::pairing::DiscoveryCapabilities;

/// What the device advertises: BLE only. WiFi credentials are what commissioning
/// delivers, so the device cannot be on WiFi before it is commissioned.
const DISCOVERY: DiscoveryCapabilities = DiscoveryCapabilities::BLE;

/// The three text forms of the onboarding data.
pub struct Onboarding {
    /// The `MT:` Base38 payload a QR code encodes.
    pub payload: String,
    /// The 11-digit manual pairing code, for entering by hand.
    pub manual_code: String,
    /// The manual code with the hyphens a commissioner UI shows.
    pub pretty_manual_code: String,
    /// Vendor ID, product ID, discriminator and passcode, for assertions.
    pub vendor_id: u16,
    pub product_id: u16,
    pub discriminator: u16,
    pub passcode: u32,
}

/// Computes the onboarding data from the firmware's commissioning constants.
pub fn onboarding() -> Onboarding {
    let payload = QrPayload::new_from_basic_info(
        DISCOVERY,
        // Standard: the device is commissionable straight out of the box with no
        // vendor-specific step.
        CommFlowType::Standard,
        TEST_DEV_COMM,
        &TEST_DEV_DET,
        no_optional_data,
    );
    // `payload.is_valid()` is deliberately not consulted. In rs-matter 0.2 its
    // vendor-ID check is inverted:
    //
    //     if VendorId::is_valid_operationally(self.vid) && self.vid != 0 { return false }
    //
    // `is_valid_operationally` is true for any vid in 1..=0xFFF4, so the guard
    // rejects every operationally valid vendor — including 0xFFF1, the test vendor
    // rs-matter itself ships in `TEST_DEV_DET`. The round-trip test in
    // tests/onboarding.rs checks the payload properly instead, by decoding it back
    // to its fields.

    let mut buffer = [0u8; 128];
    let (text, _) = payload
        .as_str(&mut buffer)
        .expect("the QR payload does not fit its buffer");

    Onboarding {
        payload: text.to_owned(),
        manual_code: TEST_DEV_COMM.compute_pairing_code().to_string(),
        pretty_manual_code: TEST_DEV_COMM.compute_pretty_pairing_code().to_string(),
        vendor_id: TEST_DEV_DET.vid,
        product_id: TEST_DEV_DET.pid,
        discriminator: TEST_DEV_COMM.discriminator,
        // The password is stored as the little-endian bytes of the passcode, which
        // is how rs-matter itself reads it back in `compute_pairing_code`.
        passcode: u32::from_le_bytes(*TEST_DEV_COMM.password.access()),
    }
}

/// Renders the payload as an SVG QR code.
///
/// Hand-rolled from the module bitmap rather than pulled from an SVG library: the
/// output has to be byte-stable so CI can diff it, and one `<rect>` per dark
/// module with no metadata, timestamps or generator comment is the simplest thing
/// that is.
pub fn svg(payload: &str) -> String {
    const QUIET_ZONE: u32 = 4;

    let mut tmp = [0u8; 2048];
    let mut out = [0u8; 2048];
    let qr = Qr::compute(payload, &mut tmp, &mut out).expect("the QR code does not fit its buffer");

    let modules = qr.size();
    let side = modules + 2 * QUIET_ZONE;

    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" \
         width=\"{px}\" height=\"{px}\" shape-rendering=\"crispEdges\" \
         role=\"img\" aria-label=\"Matter onboarding QR code\">\n",
        side = side,
        px = side * 8,
    ));
    // An explicit light background, because a transparent QR code is unreadable
    // against a dark README.
    svg.push_str(&format!(
        "  <rect width=\"{side}\" height=\"{side}\" fill=\"#ffffff\"/>\n"
    ));
    svg.push_str("  <g fill=\"#000000\">\n");
    for y in 0..modules {
        // Runs of adjacent dark modules collapse into one rect, which roughly
        // halves the file without changing what it renders.
        let mut x = 0;
        while x < modules {
            if !qr.get_module(x as i32, y as i32) {
                x += 1;
                continue;
            }
            let start = x;
            while x < modules && qr.get_module(x as i32, y as i32) {
                x += 1;
            }
            svg.push_str(&format!(
                "    <rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"1\"/>\n",
                start + QUIET_ZONE,
                y + QUIET_ZONE,
                x - start,
            ));
        }
    }
    svg.push_str("  </g>\n</svg>\n");
    svg
}

/// Renders the payload as a Unicode-block QR code.
///
/// The SVG is the pretty version, but GitHub strips inline `<svg>` from Markdown
/// and a text QR needs no image at all: it renders in a fenced code block, in a
/// terminal, in a diff, and it is still scannable from a screen. Half-block
/// characters keep it roughly square, since terminal cells are taller than wide.
pub fn text_qr(payload: &str) -> String {
    const BORDER: u8 = 2;

    let mut tmp = [0u8; 2048];
    let mut out = [0u8; 2048];
    let qr = Qr::compute(payload, &mut tmp, &mut out).expect("the QR code does not fit its buffer");

    let mut rendered = [0u8; 16 * 1024];
    // `invert: false` draws dark modules dark, which is what a scanner expects
    // against a light terminal; a dark terminal needs the inverse, and the README
    // says so next to it.
    let (text, _) = qr
        .as_str(QrTextType::Unicode, BORDER, false, &mut rendered)
        .expect("the rendered QR code does not fit its buffer");

    // rs-matter appends an ANSI reset to every line even in Unicode mode, which
    // would show up as literal `[0m` in a README. Strip CSI sequences; the
    // half-block characters carry the whole image.
    let mut clean = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            clean.push(character);
            continue;
        }
        // CSI: ESC '[' parameters, terminated by a byte in @..~
        if characters.next() == Some('[') {
            for terminator in characters.by_ref() {
                if ('@'..='~').contains(&terminator) {
                    break;
                }
            }
        }
    }
    clean
}
