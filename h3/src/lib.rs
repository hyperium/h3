pub mod client;
pub mod error;
#[deny(missing_docs)]
pub mod quic;
pub mod server;

#[allow(dead_code)]
mod connection;
mod frame;
#[allow(dead_code)]
mod qpack;
mod stream;

pub use error::Error;

#[cfg(feature = "test_helpers")]
pub use connection::ConnectionState;

#[cfg(not(feature = "test_helpers"))]
mod proto;
#[cfg(feature = "test_helpers")]
pub mod proto;
