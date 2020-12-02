use std::{convert::TryFrom, marker::PhantomData};

use bytes::{Bytes, BytesMut};
use futures::future;
use http::HeaderMap;

use crate::{
    frame::{self, FrameStream},
    proto::frame::Frame,
    proto::headers::Header,
    qpack, quic, Error,
};

pub struct ConnectionInner<C: quic::Connection<Bytes>> {
    pub(super) quic: C,
    max_field_section_size: u64,
    control_send: C::SendStream,
}

impl<C> ConnectionInner<C>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(mut quic: C, max_field_section_size: u64) -> Result<Self, Error> {
        let control_send = future::poll_fn(|mut cx| quic.poll_open_send_stream(&mut cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;

        Ok(Self {
            quic,
            control_send,
            max_field_section_size,
        })
    }
}

pub struct Builder<T> {
    pub(super) max_field_section_size: u64,
    phtantom: PhantomData<T>,
}

impl<T> Builder<T> {
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: 0, // Unlimited
            phtantom: PhantomData,
        }
    }

    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }
}

pub struct RequestStream<S, B, T> {
    pub(super) stream: S,
    pub(super) trailers: Option<Bytes>,
    _phantom_buffer: PhantomData<B>,
    _phantom_side: PhantomData<T>,
}

impl<S, B, T> RequestStream<S, B, T> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            trailers: None,
            _phantom_buffer: PhantomData,
            _phantom_side: PhantomData,
        }
    }
}

impl<S, T> RequestStream<FrameStream<S>, Bytes, T>
where
    S: quic::RecvStream,
{
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        if !self.stream.has_data() {
            match future::poll_fn(|cx| self.stream.poll_next(cx)).await? {
                Some(Frame::Data { .. }) => (),
                Some(Frame::Headers(encoded)) => {
                    self.trailers = Some(encoded);
                    return Ok(None);
                }
                Some(_) => return Err(Error::Peer("Unexpected frame type on request stream")),
                None => return Ok(None),
            }
        }

        Ok(future::poll_fn(|cx| self.stream.poll_data(cx)).await?)
    }

    /// Receive trailers
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let mut trailers = if let Some(encoded) = self.trailers.take() {
            encoded
        } else {
            match future::poll_fn(|cx| self.stream.poll_next(cx)).await? {
                Some(Frame::Headers(encoded)) => encoded,
                Some(_) => return Err(Error::Peer("Unexpected frame type on request stream")),
                None => return Ok(None),
            }
        };

        Ok(Some(
            Header::try_from(qpack::decode_stateless(&mut trailers)?)?.into_fields(),
        ))
    }
}

impl<S, T> RequestStream<S, Bytes, T>
where
    S: quic::SendStream<Bytes>,
{
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: Bytes) -> Result<(), Error> {
        frame::write(
            &mut self.stream,
            Frame::Data {
                len: buf.len() as u64,
            },
        )
        .await?;
        self.stream
            .send_data(buf)
            .map_err(|e| Error::Io(e.into()))?;
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;

        Ok(())
    }

    /// Send a set of trailers to end the request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, Header::trailer(trailers))?;

        frame::write(&mut self.stream, Frame::Headers(block.freeze())).await?;

        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| Error::Io(e.into()))
    }
}
