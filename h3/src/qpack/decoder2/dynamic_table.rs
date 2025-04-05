//! Dynamic Table for QPACK encoder
//! Instructions for this dynamic table are received from the peer encoder

use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicUsize, Arc},
};

use async_lock::RwLock;
use tokio::sync::Notify;

use crate::qpack::HeaderField;

/// Dynamic Table for QPACK decoder
/// This decoder holds information relevant to the request-streams in an RWLock
pub(super) struct MainDynamicTableForDecoder {
    /// The dynamic information relevant to the request-streams
    shared: Arc<RequestStreamDynamicTableForDecoder>,
}

impl MainDynamicTableForDecoder {
    /// Creates a new dynamic table for the decoder
    pub fn new() -> Self {
        Self {
            shared: Arc::new(RequestStreamDynamicTableForDecoder {
                table: RwLock::new(SharedDynamicTableForDecoder {
                    // TODO: Try out if HashMap is faster
                    table: BTreeMap::new(),
                }),
                blocked_streams: AtomicUsize::new(0),
                event: Notify::new(),
            }),
        }
    }
    /// Returns a reference to the shared dynamic table
    pub fn shared(&self) -> Arc<RequestStreamDynamicTableForDecoder> {
        self.shared.clone()
    }

}

/// Struct, which holds the dynamic table for the request-streams
pub(crate) struct RequestStreamDynamicTableForDecoder {
    /// The dynamic table
    pub(super) table: RwLock<SharedDynamicTableForDecoder>,
    /// Blocked streams
    pub(super) blocked_streams: AtomicUsize,
    /// Event, which is triggered when the table is updated
    pub(super) event: Notify,
}

/// Shared dynamic table for the decoder
pub(crate) struct SharedDynamicTableForDecoder {
    /// The dynamic table
    table: BTreeMap<usize, HeaderField>,
}
