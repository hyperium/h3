//! HTTP/3 client and server
#![deny(missing_docs, clippy::self_named_module_files)]
#![allow(clippy::derive_partial_eq_without_eq)]

pub mod client;
pub mod error;
pub mod quic;
pub mod server;
pub mod webtransport;

pub use error::Error;

mod buf;
mod connection;
mod frame;
mod proto;
#[allow(dead_code)]
mod qpack;
mod stream;

#[cfg(test)]
mod tests;

#[cfg(test)]
extern crate self as h3;
