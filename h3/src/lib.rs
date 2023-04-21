//! HTTP/3 client and server
#![deny(missing_docs, clippy::self_named_module_files)]
#![allow(clippy::derive_partial_eq_without_eq)]

pub mod client;
pub mod error;
pub mod quic;
pub(crate) mod request;
pub mod server;

pub use error::Error;
pub use proto::headers::Protocol;

mod buf;
#[allow(missing_docs)]
pub mod connection;
#[allow(missing_docs)]
pub mod frame;
#[allow(missing_docs)]
pub mod proto;
#[allow(dead_code)]
mod qpack;
#[allow(missing_docs)]
pub mod stream;
#[allow(missing_docs)]
pub mod webtransport;

#[cfg(test)]
mod tests;

#[cfg(test)]
extern crate self as h3;
