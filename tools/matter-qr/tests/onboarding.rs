//! Checks that the committed onboarding data describes the device that gets built.
use oci_zero_matter_qr::{onboarding, svg};

#[test]
fn the_commissioning_constants_are_the_matter_test_values() {
    let data = onboarding();

    // rs-matter's TEST_DEV_DET / TEST_DEV_COMM. Fixing them is what makes a
    // permanent QR code possible, and also what makes this device uncertifiable.
    assert_eq!(data.vendor_id, 0xFFF1, "the Matter test vendor id");
    assert_eq!(data.product_id, 0x8001, "the Matter test product id");
    assert_eq!(data.discriminator, 3840);
    assert_eq!(data.passcode, 20202021);

    // The canonical manual pairing code for that passcode and discriminator, as
    // used by chip-tool and the Matter documentation. If this number moves, the
    // commissioning constants moved with it.
    assert_eq!(data.manual_code, "34970112332");
    assert_eq!(data.pretty_manual_code, "3497-0112-332");
}

#[test]
fn the_svg_is_deterministic_and_self_contained() {
    let data = onboarding();

    // CI diffs this file against the committed one, so an unstable renderer would
    // fail every build.
    assert_eq!(
        svg(&data.payload),
        svg(&data.payload),
        "the SVG renderer is not deterministic",
    );

    let rendered = svg(&data.payload);
    assert!(rendered.starts_with("<svg xmlns="));
    assert!(rendered.trim_end().ends_with("</svg>"));
    // No external references: the README embeds this inline.
    assert!(!rendered.contains("<image"));
}
