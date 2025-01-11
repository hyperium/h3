//! This module represents the shared state of the h3 connection

use std::sync::{atomic::AtomicBool, Arc, OnceLock};

use crate::{config::Settings, error2::ConnectionError};

#[derive(Debug, Clone)]
/// This struct represents the shared state of the h3 connection and the stream structs
pub(crate) struct SharedState2 {
    /// The settings, sent by the peer
    settings: OnceLock<Settings>,
    /// The connection error
    connection_error: OnceLock<ConnectionError>,
    /// The connection is closing
    closing: Arc<AtomicBool>,
}

impl Default for SharedState2 {
    fn default() -> Self {
        Self {
            settings: OnceLock::new(),
            connection_error: OnceLock::new(),
            closing: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// This trait can be implemented for all types which have a shared state
pub trait ConnectionState2 {
    /// Get the shared state
    fn shared_state(&self) -> &SharedState2;
    /// Get the Error
    fn maybe_conn_error(&self, error: ConnectionError) -> ConnectionError {
        self.shared_state()
            .connection_error
            .get_or_init(|| error)
            .clone()
    }
    fn get_conn_error(&self) -> Option<ConnectionError> {
        self.shared_state().connection_error.get().cloned()
    }
    /// Get the settings
    fn settings(&self) -> Option<&Settings> {
        self.shared_state().settings.get()
    }
    /// Set the connection to closing
    fn set_closing(&self) {
        self.shared_state()
            .closing
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    /// Check if the connection is closing
    fn is_closing(&self) -> bool {
        self.shared_state()
            .closing
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Set the settings
    fn set_settings(&self, settings: Settings) {
        self.shared_state().settings.set(settings);
    }
}
