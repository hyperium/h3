use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Buf;
use futures_util::task::AtomicWaker;
use h3::{
    error::{Code, StreamError},
    quic,
    server::RequestStream,
    webtransport::SessionId,
};
use tokio::sync::Notify;

use super::{capsule::Decoder, SessionClose};
use crate::stream::{self, SharedStream, WeakStream, WT_SESSION_GONE};

type Outcome = Result<SessionClose, Arc<StreamError>>;

pub(super) fn gone() -> StreamError {
    StreamError::StreamError {
        code: Code::from(WT_SESSION_GONE),
        reason: "WebTransport session closed".into(),
    }
}

fn malformed(reason: &'static str) -> StreamError {
    StreamError::StreamError {
        code: Code::H3_MESSAGE_ERROR,
        reason: reason.into(),
    }
}

struct Streams<C: quic::Connection<B>, B: Buf> {
    result: Option<Outcome>,
    accepting: bool,
    terminating: bool,
    bidi: Vec<WeakStream<C::BidiStream, B>>,
    send: Vec<WeakStream<C::SendStream, B>>,
    recv: Vec<WeakStream<C::RecvStream, B>>,
}

pub(super) struct Connect<S, B> {
    pub(super) stream: RequestStream<S, B>,
    decoder: Decoder,
    pub(super) outgoing: Option<B>,
    pub(super) finishing: bool,
    pub(super) sent: bool,
    received: bool,
    pub(super) send_error: Option<Arc<StreamError>>,
}

pub(super) struct SessionState<C: quic::Connection<B>, B: Buf> {
    streams: Mutex<Streams<C, B>>,
    pub(super) connect: Mutex<Option<Connect<C::BidiStream, B>>>,
    pub(super) changed: Notify,
    driver: AtomicWaker,
}

impl<C: quic::Connection<B>, B: Buf> SessionState<C, B> {
    pub(super) fn new() -> Self {
        Self {
            streams: Mutex::new(Streams {
                result: None,
                accepting: false,
                terminating: false,
                bidi: Vec::new(),
                send: Vec::new(),
                recv: Vec::new(),
            }),
            connect: Mutex::new(None),
            changed: Notify::new(),
            driver: AtomicWaker::new(),
        }
    }

    pub(super) fn attach(&self, stream: RequestStream<C::BidiStream, B>) {
        *self.connect.lock().unwrap() = Some(Connect {
            stream,
            decoder: Decoder::default(),
            outgoing: None,
            finishing: false,
            sent: false,
            received: false,
            send_error: None,
        });
        self.streams.lock().unwrap().accepting = false;
    }

    pub(super) fn reserve(&self) -> Result<(), StreamError> {
        let mut state = self.streams.lock().unwrap();
        if state.terminating || state.result.is_some() {
            return Err(gone());
        }
        state.accepting = true;
        Ok(())
    }

    pub(super) fn is_closed(&self) -> bool {
        let state = self.streams.lock().unwrap();
        state.terminating || state.result.is_some()
    }

    pub(super) fn outcome(&self) -> Option<Outcome> {
        self.streams.lock().unwrap().result.clone()
    }

    pub(super) fn while_open<T>(&self, f: impl FnOnce() -> T) -> Result<T, StreamError> {
        let state = self.streams.lock().unwrap();
        if state.terminating || state.result.is_some() {
            return Err(gone());
        }
        Ok(f())
    }

    pub(super) fn bidi(&self, stream: &SharedStream<C::BidiStream, B>) {
        let mut state = self.streams.lock().unwrap();
        if state.terminating || state.result.is_some() {
            drop(state);
            stream::close_bidi(stream);
        } else {
            state.bidi.retain(|s| s.strong_count() != 0);
            state.bidi.push(Arc::downgrade(stream));
        }
    }

    pub(super) fn send(&self, stream: &SharedStream<C::SendStream, B>) {
        let mut state = self.streams.lock().unwrap();
        if state.terminating || state.result.is_some() {
            drop(state);
            stream::close_send(stream);
        } else {
            state.send.retain(|s| s.strong_count() != 0);
            state.send.push(Arc::downgrade(stream));
        }
    }

    pub(super) fn recv(&self, stream: &SharedStream<C::RecvStream, B>) {
        let mut state = self.streams.lock().unwrap();
        if state.terminating || state.result.is_some() {
            drop(state);
            stream::close_recv(stream);
        } else {
            state.recv.retain(|s| s.strong_count() != 0);
            state.recv.push(Arc::downgrade(stream));
        }
    }

    pub(super) fn terminate(&self, result: Outcome) {
        let (bidi, send, recv) = {
            let mut state = self.streams.lock().unwrap();
            if state.terminating || state.result.is_some() {
                return;
            }
            state.terminating = true;
            (
                std::mem::take(&mut state.bidi),
                std::mem::take(&mut state.send),
                std::mem::take(&mut state.recv),
            )
        };
        for stream in bidi.into_iter().filter_map(|s| s.upgrade()) {
            stream::close_bidi(&stream);
        }
        for stream in send.into_iter().filter_map(|s| s.upgrade()) {
            stream::close_send(&stream);
        }
        for stream in recv.into_iter().filter_map(|s| s.upgrade()) {
            stream::close_recv(&stream);
        }
        self.streams.lock().unwrap().result = Some(result);
        self.changed.notify_waiters();
        self.driver.wake();
    }

    pub(super) fn abort(&self) {
        if let Some(connect) = self.connect.lock().unwrap().as_mut() {
            if !connect.sent || !connect.received {
                if !connect.sent {
                    connect.stream.stop_stream(Code::from(WT_SESSION_GONE));
                }
                if !connect.received {
                    connect.stream.stop_sending(Code::from(WT_SESSION_GONE));
                }
                connect.sent = true;
                connect.received = true;
            }
        }
        self.terminate(Err(Arc::new(gone())));
    }

    /// Drive CONNECT control I/O without holding a lock across Pending.
    pub(super) fn poll_io(&self, cx: &mut Context<'_>) {
        let mut io = self.connect.lock().unwrap();
        let Some(connect) = io.as_mut() else {
            return;
        };
        let mut terminal = None;
        let mut send_completed = false;
        while !connect.received {
            let received = match connect.stream.poll_recv_data(cx) {
                Poll::Pending => break,
                Poll::Ready(Ok(Some(data))) => connect.decoder.feed(data),
                Poll::Ready(Ok(None)) => {
                    connect.received = true;
                    connect
                        .decoder
                        .finish()
                        .map(|()| Some(SessionClose::default()))
                }
                Poll::Ready(Err(error)) => {
                    connect.received = true;
                    connect.finishing = true;
                    terminal = Some(Err(Arc::new(error)));
                    break;
                }
            };
            match received {
                Ok(Some(close)) => {
                    connect.finishing = true;
                    if terminal.is_none() {
                        terminal = Some(Ok(close));
                    }
                }
                Ok(None) => {}
                Err(reason) => {
                    connect.stream.stop_stream(Code::H3_MESSAGE_ERROR);
                    connect.stream.stop_sending(Code::H3_MESSAGE_ERROR);
                    connect.received = true;
                    connect.sent = true;
                    let error = Arc::new(malformed(reason));
                    connect.send_error = Some(error.clone());
                    terminal = Some(Err(error));
                }
            }
        }
        if connect.finishing && !connect.sent {
            let sent = match connect.stream.poll_send_data(cx, &mut connect.outgoing) {
                Poll::Ready(Ok(())) => connect.stream.poll_finish(cx),
                result => result,
            };
            if let Poll::Ready(result) = sent {
                connect.sent = true;
                if let Err(error) = result {
                    connect.send_error = Some(Arc::new(error));
                }
                send_completed = true;
            }
        }
        drop(io);
        if let Some(result) = terminal {
            self.terminate(result);
        }
        if send_completed {
            self.changed.notify_waiters();
        }
    }

    pub(super) fn poll_driver(&self, cx: &mut Context<'_>) {
        self.driver.register(cx.waker());
        self.poll_io(cx);
    }

    fn idle(&self) -> bool {
        let io = self.connect.lock().unwrap();
        if let Some(io) = io.as_ref() {
            return io.sent && io.received;
        }
        let streams = self.streams.lock().unwrap();
        if (streams.accepting || streams.terminating) && streams.result.is_none() {
            return false;
        }
        streams.bidi.iter().all(|s| s.strong_count() == 0)
            && streams.send.iter().all(|s| s.strong_count() == 0)
            && streams.recv.iter().all(|s| s.strong_count() == 0)
    }
}

/// Active sessions and streams received before their CONNECT is accepted.
/// Retired request IDs are coalesced, rather than retaining one tombstone per session.
pub(super) struct Sessions<C: quic::Connection<B>, B: Buf> {
    active: HashMap<SessionId, Arc<SessionState<C, B>>>,
    retired: BTreeMap<u64, u64>,
}

impl<C: quic::Connection<B>, B: Buf> Sessions<C, B> {
    pub(super) fn new() -> Self {
        Self {
            active: HashMap::new(),
            retired: BTreeMap::new(),
        }
    }

    pub(super) fn get(&mut self, id: SessionId) -> Arc<SessionState<C, B>> {
        let index = u64::from(id);
        if self
            .retired
            .range(..=index)
            .next_back()
            .is_some_and(|(_, end)| *end >= index)
        {
            let state = Arc::new(SessionState::new());
            state.terminate(Err(Arc::new(gone())));
            return state;
        }
        self.active
            .entry(id)
            .or_insert_with(|| Arc::new(SessionState::new()))
            .clone()
    }

    pub(super) fn retire(&mut self, id: SessionId) -> Option<Arc<SessionState<C, B>>> {
        let removed = self.active.remove(&id);
        let mut start = u64::from(id);
        let mut end = start;
        if let Some((&s, &e)) = self.retired.range(..=start).next_back() {
            if e >= start {
                return removed;
            }
            if e + 1 == start {
                start = s;
                self.retired.remove(&s);
            }
        }
        if let Some((&s, &e)) = self.retired.range(start..).next() {
            if s <= end + 1 {
                end = e;
                self.retired.remove(&s);
            }
        }
        self.retired.insert(start, end);
        removed
    }

    pub(super) fn states(&self) -> Vec<Arc<SessionState<C, B>>> {
        self.active.values().cloned().collect()
    }

    pub(super) fn cleanup(&mut self) {
        let mut retired = Vec::new();
        self.active.retain(|id, state| {
            if state.idle() {
                if state.outcome().is_some() {
                    retired.push(*id);
                }
                return false;
            }
            true
        });
        for id in retired {
            self.retire(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn retired_ids_coalesce_across_http_requests_and_closed_sessions() {
        let mut sessions = Sessions::<h3_quinn::Connection, Bytes>::new();
        for id in (0..4096).step_by(2) {
            sessions.retire(id.try_into().unwrap());
        }
        for id in (1..4096).step_by(2) {
            sessions.retire(id.try_into().unwrap());
        }
        assert_eq!(sessions.retired.len(), 1);
        assert_eq!(sessions.retired.get(&0), Some(&4095));
        assert!(sessions
            .get(42.try_into().unwrap())
            .outcome()
            .unwrap()
            .is_err());
        assert!(sessions.active.is_empty());
        assert!(sessions.get(4096.try_into().unwrap()).outcome().is_none());
    }
}
