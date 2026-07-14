#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]
#![allow(async_fn_in_trait)]

#[cfg(any(test, feature = "docker-credentials"))]
extern crate std;

pub mod tar;

pub mod compression;
pub mod digest;
#[cfg(feature = "docker-credentials")]
pub mod docker_credentials;
mod json;
pub mod layer;
pub mod metadata;
pub mod pull;
pub mod reference;
pub mod registry;

#[cfg(feature = "reqwless")]
pub mod reqwless;

#[cfg(feature = "tls")]
pub mod tls;
