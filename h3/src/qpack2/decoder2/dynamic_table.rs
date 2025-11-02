//! Dynamic Table for QPACK encoder
//! Instructions for this dynamic table are received from the peer encoder

use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicUsize, Arc},
};

use tokio::sync::{mpsc, Notify, RwLock};

use crate::qpack::HeaderField;

/// Dynamic Table for QPACK decoder
/// This decoder holds information relevant to the request-streams in an RWLock
#[derive(Debug)]
pub(super) struct MainDynamicTableForDecoder {
    /// The dynamic information relevant to the request-streams
    shared: RequestStreamDynamicTableForDecoder,
}

impl MainDynamicTableForDecoder {
    /// Creates a new dynamic table for the decoder
    pub fn new() -> (Self, mpsc::Receiver<()>) {
        let (sender, receiver) = mpsc::channel(10);

        (
            Self {
                shared: RequestStreamDynamicTableForDecoder {
                    inner: Arc::new(InnnerDecoder {
                        table: RwLock::new(SharedDynamicTableForDecoder {
                            // TODO: Try out if HashMap is faster
                            table: BTreeMap::new(),
                        }),
                        blocked_streams: AtomicUsize::new(0),
                        event: Notify::new(),
                    }),
                    decoder_event: sender,
                },
            },
            receiver,
        )
    }
    /// Returns a reference to the shared dynamic table
    pub fn shared(&self) -> RequestStreamDynamicTableForDecoder {
        self.shared.clone()
    }
}

/// Struct, which holds the dynamic table for the request-streams
#[derive(Debug, Clone)]
pub(crate) struct RequestStreamDynamicTableForDecoder {
    /// The dynamic table
    pub(super) inner: Arc<InnnerDecoder>,
    /// Send decoder events on the decoder stream
    pub(super) decoder_event: mpsc::Sender<()>,
}

#[derive(Debug)]
pub(crate) struct InnnerDecoder {
    /// The dynamic table
    pub(super) table: RwLock<SharedDynamicTableForDecoder>,
    /// Blocked streams
    pub(super) blocked_streams: AtomicUsize,
    /// Event, which is triggered when the table is updated
    pub(super) event: Notify,
}

/// Shared dynamic table for the decoder
#[derive(Debug)]
pub(crate) struct SharedDynamicTableForDecoder {
    /// The dynamic table
    table: BTreeMap<usize, HeaderField>,
}
