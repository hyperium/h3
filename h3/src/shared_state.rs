//! This module represents the shared state of the h3 connection

use std::{
    sync::{atomic::AtomicBool, OnceLock},
    task::Poll,
};

use futures_util::task::AtomicWaker;

use crate::{
    config::Settings,
    error2::{internal_error::ErrorOrigin, ConnectionError},
};

#[derive(Debug)]
/// This struct represents the shared state of the h3 connection and the stream structs
pub struct SharedState2 {
    /// The settings, sent by the peer
    settings: OnceLock<Settings>,
    /// The connection error
    connection_error: OnceLock<ErrorOrigin>,
    /// The connection is closing
    closing: AtomicBool,
    /// Waker for the connection
    waker: AtomicWaker,
}

impl Default for SharedState2 {
    fn default() -> Self {
        Self {
            settings: OnceLock::new(),
            connection_error: OnceLock::new(),
            closing: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }
}

impl ConnectionState2 for SharedState2 {
    fn shared_state(&self) -> &SharedState2 {
        self
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
        todo!("change to poll_conn_error")
    }

    /// Polls for errors on the connection
    fn poll_conn_error(&self, cx: &mut std::task::Context<'_>) -> Poll<ErrorOrigin> {
        // Check if the connection is in error state
        if let Some(error) = self.shared_state().connection_error.get() {
            return Poll::Ready(error.clone());
        }
        // Register the waker
        self.shared_state().waker.register(cx.waker());
        Poll::Pending
    }

    /// tries to set the connection error
    /// Returns Ok(error) if the error was set by this call, Err(error) a previous error is returned
    fn try_set_conn_error(&self, error: ErrorOrigin) -> Result<ErrorOrigin, ErrorOrigin> {
        // TODO: Change when `OnceLock::try_insert` is stabilized https://github.com/rust-lang/rust/issues/116693
        let mut was_init_flag = false;
        let was_init_flag_ref = &mut was_init_flag;

        let err = self.shared_state().connection_error.get_or_init(move || {
            // Wake the connection
            self.shared_state().waker.wake();
            *was_init_flag_ref = true;
            error
        });

        if was_init_flag {
            Ok(err.clone())
        } else {
            Err(err.clone())
        }
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

#[cfg(test)]
mod test {
    use crate::error2::{internal_error::InternalConnectionError, LocalError, NewCode};
    use assert_matches::assert_matches;

    /// test the try_set_conn_error function
    #[test]
    fn test_try_set_conn_error() {
        use super::*;
        let shared_state = SharedState2::default();
        // Create 2 errors
        let error1 = ErrorOrigin::Internal(InternalConnectionError {
            code: NewCode::H3_INTERNAL_ERROR,
            message: "Test".to_string(),
        });

        let error2 = ErrorOrigin::Internal(InternalConnectionError {
            code: NewCode::H3_REQUEST_CANCELLED,
            message: "Test".to_string(),
        });
        // Set the first error
        let result = shared_state.try_set_conn_error(error1.clone());
        assert_matches!(
            result,
            Ok(ErrorOrigin::Internal(InternalConnectionError {
                code: NewCode::H3_INTERNAL_ERROR,
                message: _
            }))
        );

        // Try to set the second error
        let result = shared_state.try_set_conn_error(error2.clone());
        // should return the first error
        assert_matches!(
            result,
            Err(ErrorOrigin::Internal(InternalConnectionError {
                code: NewCode::H3_INTERNAL_ERROR,
                message: _
            }))
        );
    }
}
