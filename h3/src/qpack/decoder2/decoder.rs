//! This module is responsible for decoding Headers

use std::{sync::Arc, task::Poll};

use async_lock::futures::Read;
use ouroboros::self_referencing;
use tokio::sync::futures::Notified;

use super::dynamic_table::RequestStreamDynamicTableForDecoder;

/// Decoder for QPACK. Each RequestStream has a instance of this decoder
//#[self_referencing]
pub(crate) struct Decoder {
    /// The dynamic table
    state: Arc<RequestStreamDynamicTableForDecoder>,
    // Notifier if the request stream becomes blocked
   // #[borrows(state)]
   // #[not_covariant]
   // blocked: Option<Notified<'this>>,
}

impl Decoder {
    /*/// Creates a new decoder
    pub fn new(state: Arc<RequestStreamDynamicTableForDecoder>) -> Self {
        Self {
            state,
            blocked: None,
        }
    }*/
    fn poll_new_read(
        &mut self,
        cx: &mut std::task::Context,
    ) -> Poll<()> {
        // Poll the blocked stream
        let x = self.state.table.read();
        todo!()
    }
    /*/// Sets the notifier for the blocked stream
    pub fn set_blocked<'b>( mut self)    {
        self.state
            .blocked_streams
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.blocked = Some(self.state.event.notified());
    }*/
}
