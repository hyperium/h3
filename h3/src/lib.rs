pub mod client;
pub mod error;
#[deny(missing_docs)]
pub mod quic;
pub mod server;

mod connection;
mod frame;
mod stream;

pub use error::Error;

#[cfg(feature = "test_helpers")]
pub use connection::ConnectionState;

#[cfg(not(feature = "test_helpers"))]
mod proto;
#[cfg(feature = "test_helpers")]
pub mod proto;

#[cfg(not(feature = "test_helpers"))]
#[allow(dead_code)]
mod qpack;
#[cfg(feature = "test_helpers")]
#[allow(dead_code)]
pub mod qpack;
