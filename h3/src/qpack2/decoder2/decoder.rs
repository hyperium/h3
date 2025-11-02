//! This module is responsible for decoding Headers

use std::{sync::Arc, task::Poll};

use tokio::sync::Notify;

use super::dynamic_table::RequestStreamDynamicTableForDecoder;

/// Decoder for QPACK. Each RequestStream has a instance of this decoder
#[derive(Debug, Clone)]
pub(crate) struct Decoder {
    /// The dynamic table
    state: RequestStreamDynamicTableForDecoder,
}

impl Decoder {
    /// Creates a new decoder
    pub fn new(state: RequestStreamDynamicTableForDecoder) -> Self {
        Self { state }
    }
    fn poll_new_read(&mut self, cx: &mut std::task::Context) -> Poll<()> {
        // Poll the blocked stream
        let x = self.state.inner.table.read();
        todo!()
    }
    /// Sets the notifier for the blocked stream
    pub fn set_blocked(mut self) {
        self.state
            .inner
            .blocked_streams
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // self.blocked = Some(self.state.event.clone());
    }
}
