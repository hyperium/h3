use std::{
    convert::TryFrom,
    marker::PhantomData,
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
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

#[doc(hidden)]
pub struct SharedState {
    // maximum size for a header we send
    pub peer_max_field_section_size: u64,
    // connection-wide error, concerns all RequestStreams and drivers
    pub error: Option<Error>,
}

#[derive(Clone)]
#[doc(hidden)]
pub struct SharedStateRef(Arc<RwLock<SharedState>>);

impl SharedStateRef {
    pub fn read(&self, panic_msg: &'static str) -> RwLockReadGuard<SharedState> {
        self.0.read().expect(panic_msg)
    }

    pub fn write(&self, panic_msg: &'static str) -> RwLockWriteGuard<SharedState> {
        self.0.write().expect(panic_msg)
    }
}

impl Default for SharedStateRef {
    fn default() -> Self {
        Self(Arc::new(RwLock::new(SharedState {
            peer_max_field_section_size: VarInt::MAX.0,
            error: None,
        })))
    }
}

pub trait ConnectionState {
    fn shared_state(&self) -> &SharedStateRef;

    fn maybe_conn_err<E: Into<Error>>(&self, err: E) -> Error {
        if let Some(ref e) = self.shared_state().0.read().unwrap().error {
            e.clone()
        } else {
            err.into()
        }
    }
}

pub struct ConnectionInner<C>
where
    C: quic::Connection<Bytes>,
{
    pub(super) shared: SharedStateRef,
    conn: C,
    #[allow(dead_code)]
    control_send: C::SendStream,
    control_recv: Option<FrameStream<C::RecvStream>>,
    pending_recv_streams: Vec<AcceptRecvStream<C::RecvStream>>,
    got_peer_settings: bool,
}

impl<C> ConnectionInner<C>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(
        mut conn: C,
        max_field_section_size: u64,
        shared: SharedStateRef,
    ) -> Result<Self, Error> {
        let mut control_send = future::poll_fn(|mut cx| conn.poll_open_send(&mut cx))
            .await
            .map_err(|e| Code::H3_STREAM_CREATION_ERROR.with_transport(e))?;

        let mut settings = Settings::default();
        settings
            .insert(SettingId::MAX_HEADER_LIST_SIZE, max_field_section_size)
            .map_err(|e| Code::H3_INTERNAL_ERROR.with_cause(e))?;

        stream::write(&mut control_send, StreamType::CONTROL).await?;
        stream::write(&mut control_send, Frame::Settings(settings)).await?;

        Ok(Self {
            shared,
            conn,
            control_send,
            control_recv: None,
            pending_recv_streams: Vec::with_capacity(3),
            got_peer_settings: false,
        })
    }

    pub fn poll_accept_request(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<C::BidiStream>, Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        // .into().into() converts the impl QuicError into crate::error::Error.
        // The `?` operator doesn't work here for some reason.
        self.conn.poll_accept_bidi(cx).map_err(|e| e.into().into())
    }

    pub fn poll_accept_recv(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        loop {
            match self.conn.poll_accept_recv(cx)? {
                Poll::Ready(Some(stream)) => self
                    .pending_recv_streams
                    .push(AcceptRecvStream::new(stream)),
                Poll::Ready(None) => {
                    return Poll::Ready(Err(
                        Code::H3_GENERAL_PROTOCOL_ERROR.with_reason("Connection closed unexpected")
                    ))
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

        for (removed, index) in resolved.into_iter().enumerate() {
            let stream = self
                .pending_recv_streams
                .remove(index - removed)
                .into_stream()?;
            match stream {
                AcceptedRecvStream::Control(s) => {
                    if self.control_recv.is_some() {
                        return Poll::Ready(Err(
                            self.close(Code::H3_STREAM_CREATION_ERROR, "got two control streams")
                        ));
                    }
                    self.control_recv = Some(s);
                }
                _ => (),
            }
        }

        Poll::Pending
    }

    pub fn poll_control(&mut self, cx: &mut Context<'_>) -> Poll<Result<Frame, Error>> {
        if let Some(ref e) = self.shared.read("poll_accept_request").error {
            return Poll::Ready(Err(e.clone()));
        }

        loop {
            match self.poll_accept_recv(cx) {
                Poll::Ready(Ok(_)) => continue,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
                Poll::Pending if self.control_recv.is_none() => return Poll::Pending,
                _ => break,
            }
        }

        let recvd = ready!(self
            .control_recv
            .as_mut()
            .expect("control_recv")
            .poll_next(cx))?;

        let res = match recvd {
            None => Err(self.close(Code::H3_CLOSED_CRITICAL_STREAM, "control stream closed")),
            Some(frame) => match frame {
                Frame::Settings(settings) if !self.got_peer_settings => {
                    self.got_peer_settings = true;
                    self.shared
                        .write("connection settings write")
                        .peer_max_field_section_size = settings
                        .get(SettingId::MAX_HEADER_LIST_SIZE)
                        .unwrap_or(VarInt::MAX.0);
                    Ok(Frame::Settings(settings))
                }
                Frame::CancelPush(_) | Frame::MaxPushId(_) | Frame::Goaway(_)
                    if !self.got_peer_settings =>
                {
                    Err(self.close(
                        Code::H3_MISSING_SETTINGS,
                        format!("received {:?} before settings on control stream", frame),
                    ))
                }
                frame => Err(self.close(
                    Code::H3_FRAME_UNEXPECTED,
                    format!("on control stream: {:?}", frame),
                )),
            },
        };
        Poll::Ready(res)
    }

    pub fn close<T: AsRef<str>>(&mut self, code: Code, reason: T) -> Error {
        self.shared.write("connection close err").error = Some(code.with_reason(reason.as_ref()));
        self.conn.close(code, reason.as_ref().as_bytes());
        code.with_reason(reason.as_ref())
    }
}

pub struct RequestStream<S, B> {
    pub(super) stream: S,
    pub(super) trailers: Option<Bytes>,
    pub(super) conn_state: SharedStateRef,
    pub(super) max_field_section_size: u64,
    _phantom_buffer: PhantomData<B>,
}

impl<S, B> RequestStream<S, B> {
    pub fn new(stream: S, max_field_section_size: u64, conn_state: SharedStateRef) -> Self {
        Self {
            stream,
            conn_state,
            max_field_section_size,
            trailers: None,
            _phantom_buffer: PhantomData,
        }
    }
}

impl<S, B> ConnectionState for RequestStream<S, B> {
    fn shared_state(&self) -> &SharedStateRef {
        &self.conn_state
    }
}

impl<S> RequestStream<FrameStream<S>, Bytes>
where
    S: quic::RecvStream,
{
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<Bytes>, Error> {
        if !self.stream.has_data() {
            let frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            match frame {
                Some(Frame::Data { .. }) => (),
                Some(Frame::Headers(encoded)) => {
                    self.trailers = Some(encoded);
                    return Ok(None);
                }
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
                None => return Ok(None),
            }
        }

        let data = future::poll_fn(|cx| self.stream.poll_data(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;
        Ok(data)
    }

    /// Receive trailers
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error> {
        let mut trailers = if let Some(encoded) = self.trailers.take() {
            encoded
        } else {
            let frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;
            match frame {
                Some(Frame::Headers(encoded)) => encoded,
                Some(_) => return Err(Code::H3_FRAME_UNEXPECTED.into()),
                None => return Ok(None),
            }
        };

        if !self.stream.is_eos() {
            // Get the trailing frame
            let trailing_frame = future::poll_fn(|cx| self.stream.poll_next(cx))
                .await
                .map_err(|e| self.maybe_conn_err(e))?;

            if let Some(_) = trailing_frame {
                // if it's not unknown or reserved, fail.
                return Err(Code::H3_FRAME_UNEXPECTED.into());
            }
        }

        let qpack::Decoded {
            fields, mem_size, ..
        } = qpack::decode_stateless(&mut trailers)?;
        if mem_size > self.max_field_section_size {
            return Err(Error::header_too_big(mem_size, self.max_field_section_size));
        }

        Ok(Some(Header::try_from(fields)?.into_fields()))
    }

    pub fn stop_sending(&mut self, err_code: Code) {
        self.stream.stop_sending(err_code);
    }
}

impl<S> RequestStream<S, Bytes>
where
    S: quic::SendStream<Bytes>,
{
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: Bytes) -> Result<(), Error> {
        let frame = Frame::Data {
            len: buf.len() as u64,
        };
        stream::write(&mut self.stream, frame)
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        self.stream
            .send_data(buf)
            .map_err(|e| self.maybe_conn_err(e))?;
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    /// Send a set of trailers to end the request.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error> {
        let mut block = BytesMut::new();
        let mem_size = qpack::encode_stateless(&mut block, Header::trailer(trailers))?;
        let max_mem_size = self
            .conn_state
            .read("send_trailers shared state read")
            .peer_max_field_section_size;
        if mem_size > max_mem_size {
            return Err(Error::header_too_big(mem_size, max_mem_size));
        }

        stream::write(&mut self.stream, Frame::Headers(block.freeze()))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;

        Ok(())
    }

    pub async fn finish(&mut self) -> Result<(), Error> {
        future::poll_fn(|cx| self.stream.poll_ready(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))?;
        future::poll_fn(|cx| self.stream.poll_finish(cx))
            .await
            .map_err(|e| self.maybe_conn_err(e))
    }
}
