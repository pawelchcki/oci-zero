#![doc = include_str!("../README.md")]
#![no_std]
#![forbid(unsafe_code)]
#![allow(async_fn_in_trait)]

#[cfg(test)]
extern crate std;

pub mod tar;

pub mod compression;
pub mod digest;
mod json;
pub mod layer;
pub mod metadata;
pub mod pull;
pub mod reference;
pub mod registry;
