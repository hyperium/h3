pub use self::{
    decoder::{ack_header, stream_canceled, Decoder, Error as DecoderError},
    dynamic::Error as DynamicTableError,
    encoder::{Encoder, Error as EncoderError},
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

#[derive(Debug)]
pub enum Error {
    Encoder(EncoderError),
    Decoder(DecoderError),
}
