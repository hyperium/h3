use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::{
    proto::{stream::StreamId, varint::VarInt},
    Error,
};

#[doc(hidden)]
pub struct SharedState {
    // maximum size for a header we send
    pub peer_max_field_section_size: u64,
    // connection-wide error, concerns all RequestStreams and drivers
    pub error: Option<Error>,
    // Has the connection received a GoAway frame? If so, this StreamId is the last
    // we're willing to accept. This lets us finish the requests or pushes that were
    // already in flight when the graceful shutdown was initiated.
    pub closing: Option<StreamId>,
    /// Stream ID of the last accepted Stream
    pub last_accepted_stream: Option<StreamId>,
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
            closing: None,
            last_accepted_stream: None,
        })))
    }
}

pub trait ConnectionState {
    fn shared_state(&self) -> &SharedStateRef;

    /// checks if there has been an connection error somewhere and returns the Connection  Error.
    /// If there was no Connection Error it returns the given Error
    fn maybe_conn_err<E: Into<Error>>(&self, err: E) -> Error {
        if let Some(ref e) = self.shared_state().0.read().unwrap().error {
            e.clone()
        } else {
            err.into()
        }
    }
}
