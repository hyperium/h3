//! HTTP/3 client and server
#[deny(missing_docs)]
pub mod client;
#[deny(missing_docs)]
pub mod error;
#[deny(missing_docs)]
pub mod quic;
#[deny(missing_docs)]
pub mod server;

pub use error::Error;

mod buf;
mod connection;
mod frame;
mod proto;
#[allow(dead_code)]
mod qpack;
mod stream;

#[cfg(feature = "test_helpers")]
#[doc(hidden)]
pub mod test_helpers {
    pub mod qpack {
        pub use crate::qpack::*;
    }
    pub mod proto {
        pub use crate::proto::*;
    }

    pub use super::connection::ConnectionState;
}
