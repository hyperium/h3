#[allow(dead_code)]
pub mod client;
#[deny(missing_docs)]
pub mod quic;
#[allow(dead_code)]
pub mod server;

// TODO: remove once methods are effectively used through public API.
#[allow(dead_code)]
mod connection;
#[allow(dead_code)]
mod frame;
mod proto;
#[allow(dead_code)]
mod qpack;

#[derive(Debug)]
pub enum Error {
    Io(Box<dyn std::error::Error + Send + Sync>),
    Qpack(qpack::Error),
}

impl From<qpack::EncoderError> for Error {
    fn from(e: qpack::EncoderError) -> Self {
        Error::Qpack(qpack::Error::Encoder(e))
    }
}
