pub use self::{
    decoder::{decode_stateless, Decoded, DecoderError},
    encoder::{encode_stateless, EncoderError},
    field::HeaderField,
};

mod block;
mod dynamic;
mod field;
mod parse_error;
mod static_;
mod stream;
mod vas;

mod decoder;
mod encoder;

mod prefix_int;
mod prefix_string;

#[cfg(test)]
mod tests;

/// Test helper: encodes a stateless QPACK field section from raw `(name, value)` pairs,
/// bypassing `Header` construction/validation. Used by integration tests that must emit
/// malformed field sections (e.g. connection-specific headers) that the public API rejects.
#[cfg(test)]
pub(crate) fn encode_raw_header_block<W: bytes::BufMut>(
    buf: &mut W,
    fields: &[(&[u8], &[u8])],
) {
    use block::{HeaderPrefix, Literal};
    HeaderPrefix::new(0, 0, 0, 0).encode(buf);
    for (name, value) in fields {
        Literal::new(name.to_vec(), value.to_vec())
            .encode(buf)
            .unwrap();
    }
}

#[derive(Debug)]
pub enum Error {
    Encoder(EncoderError),
    Decoder(DecoderError),
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Encoder(e) => write!(f, "Encoder {}", e),
            Error::Decoder(e) => write!(f, "Decoder {}", e),
        }
    }
}
