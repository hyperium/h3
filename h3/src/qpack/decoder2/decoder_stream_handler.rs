//! This module is responsible for handling decoder_send and encoder_recv streams

use std::task::{ready, Poll};

use crate::{error::internal_error::InternalConnectionError, quic, stream::BufRecvStream};

pub(crate) struct DecoderStreamHandler<C, B>
where
    C: quic::Connection<B>,
    B: bytes::Buf,
{
    decoder_send: C::SendStream,
    pub(crate) encoder_recv: Option<BufRecvStream<C::RecvStream, B>>,
    table: super::dynamic_table::MainDynamicTableForDecoder,
   // future: F,

}

impl<C, B> DecoderStreamHandler<C, B>
where
    C: quic::Connection<B>,
    B: bytes::Buf,
{
    /// Creates a new decoder stream handler
    pub fn new(decoder_send: C::SendStream) -> Self {
        Self {
            decoder_send,
            encoder_recv: None,
            table: super::dynamic_table::MainDynamicTableForDecoder::new(),
        }
    }
    /// Returns a Decoder with a link to the same dynamic table
    pub fn get_decoder(&self) -> super::decoder::Decoder {
        //   super::decoder::Decoder::new(self.table.shared())
        todo!()
    }

    /// Listen for incoming data on the encoder_recv stream
    pub fn poll_incoming_encoder_instructions(
        &mut self,
        cx: &mut std::task::Context,
    ) -> Poll<Result<(), InternalConnectionError>> {
        // Poll the encoder_recv stream for incoming data
        /*ready!(self.encoder_recv.poll_read(cx)).map_err(|e| {
            todo!("Handle error: {}", e);
        })?;

        */
        todo!("Handle incoming encoder instructions");
    }
}
