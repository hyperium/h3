//! HTTP/3 client and server
#![deny(missing_docs, clippy::self_named_module_files)]
#![allow(clippy::derive_partial_eq_without_eq)]

pub mod client;
pub mod error;
pub mod ext;
pub mod quic;
pub(crate) mod request;
pub mod server;

pub use error::Error;

mod buf;

#[cfg(feature = "allow_access_to_core")]
#[allow(missing_docs)]
pub mod connection;
#[cfg(feature = "allow_access_to_core")]
#[allow(missing_docs)]
pub mod frame;
#[cfg(feature = "allow_access_to_core")]
#[allow(missing_docs)]
pub mod proto;
#[cfg(feature = "allow_access_to_core")]
#[allow(missing_docs)]
pub mod stream;
#[cfg(feature = "allow_access_to_core")]
#[allow(missing_docs)]
pub mod webtransport;

#[cfg(not(feature = "allow_access_to_core"))]
mod connection;
#[cfg(not(feature = "allow_access_to_core"))]
mod frame;
#[cfg(not(feature = "allow_access_to_core"))]
mod proto;
#[cfg(not(feature = "allow_access_to_core"))]
mod stream;
#[cfg(not(feature = "allow_access_to_core"))]
mod webtransport;

#[allow(dead_code)]
mod qpack;
#[cfg(test)]
mod tests;
#[cfg(test)]
extern crate self as h3;
