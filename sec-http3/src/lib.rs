//! HTTP/3 client and server
#![deny(missing_docs, clippy::self_named_module_files)]
#![allow(clippy::derive_partial_eq_without_eq)]

pub mod client;
mod config;
pub mod error;
pub mod ext;
pub mod quic;
pub(crate) mod request;
pub mod server;

pub use error::Error;

pub mod sec_http3_quinn;
#[allow(missing_docs)]
pub mod webtransport;

mod buf;

mod connection;
mod frame;
mod proto;
mod stream;

#[allow(dead_code)]
mod qpack;
#[cfg(test)]
mod tests;
#[cfg(test)]
extern crate self as h3;
