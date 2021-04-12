use std::{
    convert::TryFrom,
    marker::PhantomData,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use futures::{future, ready};
use http::HeaderMap;

use crate::{
    error::{Code, Error},
    frame::FrameStream,
    proto::{
        frame::{Frame, SettingId, Settings},
        stream::StreamType,
    },
    proto::{headers::Header, varint::VarInt},
    qpack, quic, stream,
    stream::{AcceptRecvStream, AcceptedRecvStream},
};

pub struct ConnectionInner<C: quic::Connection<Bytes>> {
    pub(super) quic: C,
    max_field_section_size: u64,
    peer_max_field_section_size: u64,
    control_send: C::SendStream,
    control_recv: Option<FrameStream<C::RecvStream>>,
    pending_recv_streams: Vec<AcceptRecvStream<C::RecvStream>>,
    got_peer_settings: bool,
}

impl<C> ConnectionInner<C>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(mut quic: C, max_field_section_size: u64) -> Result<Self, Error> {
        let mut control_send = future::poll_fn(|mut cx| quic.poll_open_send_stream(&mut cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_cause(e))?;

        let mut settings = Settings::default();
        settings
            .insert(SettingId::MAX_HEADER_LIST_SIZE, max_field_section_size)
            .map_err(|e| Code::H3_INTERNAL_ERROR.with_cause(e))?;

        stream::write(&mut control_send, StreamType::CONTROL).await?;
        stream::write(&mut control_send, Frame::Settings(settings)).await?;

        Ok(Self {
            quic,
            control_send,
            max_field_section_size,
            peer_max_field_section_size: VarInt::MAX.0,
            control_recv: None,
            pending_recv_streams: Vec::with_capacity(3),
            got_peer_settings: false,
        })
    }

    pub(crate) fn poll_accept_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        loop {
            match self
                .quic
                .poll_accept_recv_stream(cx)
                .map_err(|e| Error::transport(e))?
            {
                Poll::Ready(Some(stream)) => self
                    .pending_recv_streams
                    .push(AcceptRecvStream::new(stream)),
                Poll::Ready(None) => {
                    return Err(Error::transport("Connection closed unexpected")).into()
                }
                Poll::Pending => break,
            }
        }

        let mut resolved = vec![];

        for (index, pending) in self.pending_recv_streams.iter_mut().enumerate() {
            match pending.poll_type(cx)? {
                Poll::Ready(()) => resolved.push(index),
                Poll::Pending => (),
            }
        }

        for index in resolved {
            match self.pending_recv_streams.remove(index).into_stream()? {
                AcceptedRecvStream::Control(s) => {
                    self.control_recv = Some(s);
                }
                _ => (),
            }
        }

        Poll::Pending
    }

    pub fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<Frame, Error>> {
        while self.control_recv.is_none() {
            ready!(self.poll_accept_recv(cx))?;
        }

        let recvd = ready!(self
            .control_recv
            .as_mut()
            .expect("control_recv")
            .poll_next(cx))?;

        let res = match recvd {
            None => Err(Code::H3_CLOSED_CRITICAL_STREAM.with_reason("control stream closed")),
            Some(frame) => match frame {
                Frame::Settings(settings) => {
                    if self.got_peer_settings {
                        Err(Code::H3_FRAME_UNEXPECTED
                            .with_reason("settings frame already received"))
                    } else {
                        self.got_peer_settings = true;
                        self.peer_max_field_section_size = settings
                            .get(SettingId::MAX_HEADER_LIST_SIZE)
                            .unwrap_or(VarInt::MAX.0);
                        Ok(Frame::Settings(settings))
                    }
                }
                f @ Frame::CancelPush(_) | f @ Frame::Goaway(_) | f @ Frame::MaxPushId(_) => {
                    if self.got_peer_settings {
                        Ok(f)
                    } else {
                        Err(Code::H3_MISSING_SETTINGS.into())
                    }
                }
                frame => Err(Code::H3_FRAME_UNEXPECTED
                    .with_reason(format!("on control stream: {:?}", frame))),
            },
        };
        Poll::Ready(res)
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

pub struct RequestStream<S, B> {
    pub(super) stream: S,
    pub(super) trailers: Option<Bytes>,
    _phantom_buffer: PhantomData<B>,
}

impl<S, B> RequestStream<S, B> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            trailers: None,
            _phantom_buffer: PhantomData,
        }
    }
}

impl<S> RequestStream<FrameStream<S>, Bytes>
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
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
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
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
                None => return Ok(None),
            }
        };

        Ok(Some(
            Header::try_from(qpack::decode_stateless(&mut trailers)?)?.into_fields(),
        ))
    }
}

impl<S> RequestStream<S, Bytes>
where
    S: quic::SendStream<Bytes>,
{
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: Bytes) -> Result<(), Error> {
        stream::write(
            &mut self.stream,
            Frame::Data {
                len: buf.len() as u64,
            },
        )
        .await?;
        self.stream
            .send_data(buf)
            .map_err(|e| Code::H3_GENERAL_PROTOCOL_ERROR.with_cause(e))?;
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Code::H3_GENERAL_PROTOCOL_ERROR.with_cause(e))?;

        Ok(())
    }

    /// Send a set of trailers to end the request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        qpack::encode_stateless(&mut block, Header::trailer(trailers))?;

        stream::write(&mut self.stream, Frame::Headers(block.freeze())).await?;

        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| Code::H3_GENERAL_PROTOCOL_ERROR.with_cause(e))?;
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| Code::H3_GENERAL_PROTOCOL_ERROR.with_cause(e))
    }
}
