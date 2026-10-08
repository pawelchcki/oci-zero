# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.1](https://github.com/pawelchcki/oci-zero/compare/oci-zero-v0.2.0...oci-zero-v0.2.1) - 2026-10-08

### Added

- *(oci-zero)* serve in-memory registries with local and worker demos ([#22](https://github.com/pawelchcki/oci-zero/pull/22))

### Fixed

- reject unsafe archive and artifact inputs ([#11](https://github.com/pawelchcki/oci-zero/pull/11))

## [0.2.0](https://github.com/pawelchcki/oci-zero/compare/oci-zero-v0.1.0...oci-zero-v0.2.0) - 2026-10-07

### Added

- *(zstd-zero)* [**breaking**] move entropy workspaces into caller-owned buffers ([#16](https://github.com/pawelchcki/oci-zero/pull/16))
- *(oci-zero-esp32c3-ota)* commission over BLE with a permanent QR code

### Fixed

- *(oci-zero-web)* let a media type wrap in the descriptor row ([#10](https://github.com/pawelchcki/oci-zero/pull/10))
- *(oci-zero-esp32c3-ota)* give each board its own Matter UniqueID
- *(oci-zero-web)* reset native-USB boards with the USB-Serial-JTAG sequence
- *(oci-zero-esp32c3-ota)* stamp the artifact version into the app descriptor
- *(oci-zero-web)* erase the flash before writing a new partition table
- *(oci-zero-esp32c3-ota)* give the node a device type so commissioners show it
- *(oci-zero-esp32c3-ota)* ship the bootloader and partition table in the artifact
- *(zstd-zero)* reject truncated Huffman literal bitstreams
