//! This module is responsible for handling decoder_send and encoder_recv streams

use std::task::{ready, Poll};

use tokio::sync::mpsc;
use tracing::event;

use crate::{
    error::internal_error::InternalConnectionError,
    quic::{self, RecvStream},
    stream::BufRecvStream,
};

use super::{decoder, dynamic_table::MainDynamicTableForDecoder};

pub(crate) struct DecoderStreamHandler<C, B>
where
    C: quic::Connection<B>,
    B: bytes::Buf,
{
    decoder_send: C::SendStream,
    pub(crate) encoder_recv: BufRecvStream<C::RecvStream, B>,
    table: super::dynamic_table::MainDynamicTableForDecoder,
    /// When decoder events are triggered, this channel is used to notify the decoder
    decoder_event: mpsc::Receiver<()>,
}

impl<C, B> DecoderStreamHandler<C, B>
where
    C: quic::Connection<B>,
    B: bytes::Buf,
{
    /// Creates a new decoder stream handler
    pub fn new(decoder_send: C::SendStream, encoder_recv: BufRecvStream<C::RecvStream, B>) -> Self {
        let (table, event) = super::dynamic_table::MainDynamicTableForDecoder::new();

        Self {
            decoder_send,
            encoder_recv,
            table: table,
            decoder_event: event,
        }
    }
    /// Returns a Decoder with a link to the same dynamic table
    pub fn get_decoder(&self) -> super::decoder::Decoder {
        super::decoder::Decoder::new(self.table.shared())
    }

    /// Function to handle the encoder streams
    async fn handle_qpack_decoder(self) -> Result<(), InternalConnectionError> {
        let Self {
            decoder_send,
            encoder_recv,
            table,
            decoder_event,
        } = self;

        Err(tokio::select! {
            err = recv_encoder_instruction(table, encoder_recv) => err,
            err = send_decoder_instructions(decoder_send, decoder_event) => err,
        })
    }
}

/// Function to handle the encoder_recv stream
///
/// This function is responsible for receiving the encoder instructions from the peer
/// The future resolves only in the event of an error
async fn recv_encoder_instruction<S>(
    table: MainDynamicTableForDecoder,
    encoder_recv: S,
) -> InternalConnectionError
where
    S: RecvStream,
{
    loop {
        // Wait for encoder instructions from peer

        // Decode the instructions

        // Update the dynamic table
    }
}

/// Function to handle the decoder_send stream
///
/// This function is responsible for sending the decoder instructions to the peer
/// The future resolves only in the event of an error
async fn send_decoder_instructions<S, B>(
    decoder_send: S,
    decoder_event: mpsc::Receiver<()>,
) -> InternalConnectionError
where
    S: quic::SendStream<B>,
    B: bytes::Buf,
{
    loop {
        // Wait for decoder events

        // Send the decoder instructions
    }
}
