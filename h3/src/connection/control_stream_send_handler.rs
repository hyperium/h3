use std::marker::PhantomData;

use bytes::Buf;

use crate::{
    proto::{frame::Frame, stream::StreamId},
    stream, Error,
};

use super::connection_state::SharedStateRef;

pub struct ControlStreamSendHandler<S, B>
where
    S: crate::quic::SendStream<B>,
    B: Buf,
{
    pub(crate) shared: SharedStateRef,
    pub(super) control_send: S,
    pub(super) buf: PhantomData<B>,
}

impl<S, B> ControlStreamSendHandler<S, B>
where
    S: crate::quic::SendStream<B>,
    B: Buf,
{
    /// Initiate graceful shutdown, accepting `max_streams` potentially in-flight streams
    pub async fn shutdown(&mut self, max_streams: usize) -> Result<(), Error> {
        let max_id = self
            .shared
            .read("failed to read last_accepted_stream")
            .last_accepted_stream
            .map(|id| id + max_streams)
            .unwrap_or_else(StreamId::first_request);

        self.shared.write("graceful shutdown").closing = Some(max_id);

        //= https://www.rfc-editor.org/rfc/rfc9114#section-3.3
        //# When either endpoint chooses to close the HTTP/3
        //# connection, the terminating endpoint SHOULD first send a GOAWAY frame
        //# (Section 5.2) so that both endpoints can reliably determine whether
        //# previously sent frames have been processed and gracefully complete or
        //# terminate any necessary remaining tasks.
        stream::write(&mut self.control_send, Frame::Goaway(max_id)).await
    }
}
