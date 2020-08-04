//! QUIC Transport implementation with Quinn
//!
//! This module implements QUIC traits with Quinn.
use std::{
    collections::BTreeMap,
    task::{self, Poll},
};

use futures::{ready, FutureExt, StreamExt};

use bytes::{Buf, Bytes};
use quinn::{
    generic::{IncomingBiStreams, IncomingUniStreams, NewConnection, OpenBi, OpenUni},
    ConnectionError, ReadError, VarInt, WriteError,
};
use quinn_proto::crypto::Session;

use h3::quic;

pub struct Connection<S: Session> {
    conn: quinn::generic::Connection<S>,
    incoming_bi: IncomingBiStreams<S>,
    opening_bi: Option<OpenBi<S>>,
    incoming_uni: IncomingUniStreams<S>,
    opening_uni: Option<OpenUni<S>>,
}

impl<S: Session> Connection<S> {
    pub fn new(new_conn: NewConnection<S>) -> Self {
        let NewConnection {
            uni_streams,
            bi_streams,
            connection,
            ..
        } = new_conn;

        Self {
            conn: connection,
            incoming_bi: bi_streams,
            opening_bi: None,
            incoming_uni: uni_streams,
            opening_uni: None,
        }
    }
}

impl<B, S> quic::Connection<B> for Connection<S>
where
    B: Buf,
    S: Session,
{
    type SendStream = SendStream<B, S>;
    type RecvStream = RecvStream<S>;
    type BidiStream = BidiStream<B, S>;
    type Error = ConnectionError;

    fn poll_accept_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::BidiStream>, Self::Error>> {
        let (send, recv) = match ready!(self.incoming_bi.next().poll_unpin(cx)) {
            Some(x) => x?,
            None => return Poll::Ready(Ok(None)),
        };
        Poll::Ready(Ok(Some(Self::BidiStream {
            send: Self::SendStream::new(send),
            recv: Self::RecvStream::new(recv),
        })))
    }

    fn poll_open_bidi_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::BidiStream, Self::Error>> {
        if self.opening_bi.is_none() {
            self.opening_bi = Some(self.conn.open_bi());
        }

        let (send, recv) = ready!(self.opening_bi.as_mut().unwrap().poll_unpin(cx))?;
        Poll::Ready(Ok(Self::BidiStream {
            send: Self::SendStream::new(send),
            recv: Self::RecvStream::new(recv),
        }))
    }

    fn poll_accept_recv_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::RecvStream>, Self::Error>> {
        let recv = match ready!(self.incoming_uni.poll_next_unpin(cx)) {
            Some(x) => x?,
            None => return Poll::Ready(Ok(None)),
        };
        Poll::Ready(Ok(Some(Self::RecvStream::new(recv))))
    }

    fn poll_open_send_stream(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        if self.opening_uni.is_none() {
            self.opening_uni = Some(self.conn.open_uni());
        }

        let send = ready!(self.opening_uni.as_mut().unwrap().poll_unpin(cx))?;
        Poll::Ready(Ok(Self::SendStream::new(send)))
    }
}

pub struct BidiStream<B, S>
where
    B: Buf,
    S: Session,
{
    send: SendStream<B, S>,
    recv: RecvStream<S>,
}

impl<B, S> quic::BidiStream<B> for BidiStream<B, S>
where
    B: Buf,
    S: Session,
{
    type SendStream = SendStream<B, S>;
    type RecvStream = RecvStream<S>;

    fn split(self) -> (Self::SendStream, Self::RecvStream) {
        return (self.send, self.recv);
    }
}

impl<B, S> quic::RecvStream for BidiStream<B, S>
where
    B: Buf,
    S: Session,
{
    type Buf = Bytes;
    type Error = ReadError;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        self.recv.poll_data(cx)
    }

    fn stop_sending(&mut self, error_code: u64) {
        self.recv.stop_sending(error_code)
    }
}

impl<B, S> quic::SendStream<B> for BidiStream<B, S>
where
    B: Buf,
    S: Session,
{
    type Error = SendStreamError;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_ready(cx)
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.send.poll_finish(cx)
    }

    fn reset(&mut self, reset_code: u64) {
        self.send.reset(reset_code)
    }

    fn send_data(&mut self, data: B) -> Result<(), Self::Error> {
        self.send.send_data(data)
    }
}

pub struct RecvStream<S: Session> {
    stream: quinn::generic::RecvStream<S>,
    offset: u64,
    chunks: BTreeMap<u64, Bytes>,
}

impl<S: Session> RecvStream<S> {
    fn new(stream: quinn::generic::RecvStream<S>) -> Self {
        Self {
            stream,
            offset: 0,
            chunks: BTreeMap::new(),
        }
    }
}

impl<S: Session> quic::RecvStream for RecvStream<S> {
    type Buf = Bytes;
    type Error = ReadError;

    fn poll_data(
        &mut self,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<Option<Self::Buf>, Self::Error>> {
        let ret = match self.stream.read_unordered().poll_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(None)) => Poll::Ready(Ok(None)),
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
            // If we get the chunk we're looking for, return it right away
            Poll::Ready(Ok(Some((mut chunk, offset)))) if offset <= self.offset => {
                chunk.advance((self.offset - offset) as usize); // XXX overflow
                self.offset += chunk.len() as u64;
                return Poll::Ready(Ok(Some(chunk)));
            }
            // A chunk beyond current offset gets saved
            Poll::Ready(Ok(Some((data, offset)))) => {
                self.chunks.insert(offset, data);
                Poll::Pending
            }
        };

        // Nothing we've read can be yeilded, but we could have some chunk corresponding to `offset`
        let chunk_key = self
            .chunks
            .keys()
            .take_while(|x| **x <= self.offset)
            .next()
            .map(|x| *x);
        if let Some(offset) = chunk_key {
            let mut chunk = self.chunks.remove(&offset).unwrap();
            chunk.advance((self.offset - offset) as usize); // XXX overflow
            self.offset += chunk.len() as u64;
            return Poll::Ready(Ok(Some(chunk)));
        };

        ret
    }

    fn stop_sending(&mut self, error_code: u64) {
        let _ = self
            .stream
            .stop(VarInt::from_u64(error_code).expect("invalid error_code"));
    }
}

pub struct SendStream<B: Buf, S: Session> {
    stream: quinn::generic::SendStream<S>,
    writing: Option<B>,
}

impl<B, S> SendStream<B, S>
where
    B: Buf,
    S: Session,
{
    fn new(stream: quinn::generic::SendStream<S>) -> SendStream<B, S> {
        Self {
            stream,
            writing: None,
        }
    }
}

impl<B, S> quic::SendStream<B> for SendStream<B, S>
where
    B: Buf,
    S: Session,
{
    type Error = SendStreamError;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let Some(ref mut data) = self.writing {
            ready!(self.stream.write_all(data.bytes()).poll_unpin(cx))?;
        }
        self.writing = None;
        Poll::Ready(Ok(()))
    }

    fn poll_finish(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.stream.finish().poll_unpin(cx).map_err(Into::into)
    }

    fn reset(&mut self, reset_code: u64) {
        let _ = self
            .stream
            .reset(VarInt::from_u64(reset_code).unwrap_or(VarInt::MAX));
    }

    fn send_data(&mut self, data: B) -> Result<(), Self::Error> {
        if self.writing.is_some() {
            return Err(Self::Error::NotReady);
        }
        self.writing = Some(data);
        Ok(())
    }
}

// TODO bound to stdError ?
pub enum SendStreamError {
    Write(WriteError),
    NotReady,
}

impl From<WriteError> for SendStreamError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}
