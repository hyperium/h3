//! QUIC Transport traits
//!
//! This module includes traits and types meant to allow being generic over any
//! QUIC implementation.

use std::task::{self, Poll};

use bytes::Buf;

// Unresolved questions:
//
// - Should the `poll_` methods be `Pin<&mut Self>`?

pub trait Connection<B: Buf> {
    type SendStream: SendStream<B>;
    type RecvStream: RecvStream;
    type BidiStream: SendStream<B> + RecvStream;
    type Error;

    // Accepting streams

    fn poll_accept_bidi_stream(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<Self::BidiStream, Self::Error>>;

    fn poll_accept_uni_stream(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<Self::RecvStream, Self::Error>>;

    // Creating streams

    fn poll_open_bidi_stream(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<Self::BidiStream, Self::Error>>;

    fn poll_open_uni_stream(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<Self::SendStream, Self::Error>>;
}

pub trait SendStream<B: Buf> {
    type Error; // bounds?

    fn poll_ready(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<(), Self::Error>>;

    fn try_send_data(&mut self, data: B) -> Result<(), Self::Error>;

    fn reset(&mut self, reset_code: u64) -> Result<(), Self::Error>;
}

pub trait RecvStream {
    type Buf: Buf;
    type Error; // bounds?

    fn poll_data(&mut self, cx: &mut task::Context<'_>)
        -> Poll<Result<Option<Self::Buf>, Self::Error>>;

    fn stop_sending(&mut self, error_code: u64);
}

/// Optional trait to allow "splitting" a bidirectional stream into two sides.
pub trait BidiStream<B: Buf>: SendStream<B> + RecvStream {
    type SendStream: SendStream<B>;
    type RecvStream: RecvStream;
    /*
    fn split(self) -> 
    */
}

// Ext

pub(crate) async fn open_uni_stream<T, B>(_conn: &mut T)
    -> Result<T::SendStream, T::Error>
where
    T: Connection<B>,
    B: Buf,
{
    todo!()
}

pub(crate) async fn accept_uni_stream<T, B>(_conn: &mut T)
    -> Result<T::RecvStream, T::Error>
where
    T: Connection<B>,
    B: Buf,
{
    todo!()
}

pub(crate) async fn send_data<T, B>(_stream: &mut T, _buf: B)
    -> Result<(), T::Error>
where
    T: SendStream<B>,
    B: Buf,
{
    todo!()
}

pub(crate) async fn recv_data<T>(_stream: &mut T)
    -> Result<T::Buf, T::Error>
where
    T: RecvStream,
{
    todo!()
}
