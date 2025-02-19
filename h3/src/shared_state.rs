//! This module represents the shared state of the h3 connection

use std::sync::{atomic::AtomicBool, OnceLock};

use crate::{config::Settings, error2::ConnectionError};

#[derive(Debug)]
/// This struct represents the shared state of the h3 connection and the stream structs
pub struct SharedState2 {
    /// The settings, sent by the peer
    settings: OnceLock<Settings>,
    /// The connection error
    connection_error: OnceLock<ConnectionError>,
    /// The connection is closing
    closing: AtomicBool,
}

impl Default for SharedState2 {
    fn default() -> Self {
        Self {
            settings: OnceLock::new(),
            connection_error: OnceLock::new(),
            closing: AtomicBool::new(false),
        }
    }
}

/// This trait can be implemented for all types which have a shared state
pub trait ConnectionState2 {
    /// Get the shared state
    fn shared_state(&self) -> &SharedState2;
    /// Get the connection error if the connection is in error state because of another task
    ///
    /// Return the error as an Err variant if it is set in order to allow using ? in the calling function
    fn get_conn_error(&self) -> Result<(), ConnectionError> {
        if let Some(err) = self.shared_state().connection_error.get() {
            Err(err.clone())
        } else {
            Ok(())
        }
    }
    /// Set the connection error
    fn set_conn_error(&self, error: ConnectionError) {
        self.shared_state().connection_error.set(error);
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
