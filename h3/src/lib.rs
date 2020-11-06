pub mod client;
#[deny(missing_docs)]
pub mod quic;
pub mod server;

#[allow(dead_code)]
mod connection;
mod frame;
mod proto;
#[allow(dead_code)]
mod qpack;

#[derive(Debug)]
pub enum Error {
    Io(Box<dyn std::error::Error + Send + Sync>),
    Qpack(qpack::Error),
    Peer(&'static str),
    Frame(proto::frame::Error),
    Header(proto::headers::Error),
}

impl From<qpack::EncoderError> for Error {
    fn from(e: qpack::EncoderError) -> Self {
        Error::Qpack(qpack::Error::Encoder(e))
    }
}

impl From<qpack::DecoderError> for Error {
    fn from(e: qpack::DecoderError) -> Self {
        Error::Qpack(qpack::Error::Decoder(e))
    }
}

impl From<proto::headers::Error> for Error {
    fn from(e: proto::headers::Error) -> Self {
        Error::Header(e)
    }
}

impl From<frame::Error> for Error {
    fn from(e: frame::Error) -> Self {
        match e {
            frame::Error::Quic(e) => Error::Io(e),
            frame::Error::Proto(e) => Error::Frame(e),
            frame::Error::UnexpectedEnd => Error::Peer("Received incomplete frame"),
        }
    }
}
