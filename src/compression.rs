//! Optional compression decoder integrations.

#[cfg(feature = "gzip")]
pub use gzip_zero as gzip;

#[cfg(feature = "zstd")]
pub use zstd_zero as zstd;
