//! Defines error traits

use crate::config::Config;

use super::{internal_error::InternalError, ConnectionError};

/// This trait is implemented for all types which can close the connection
pub(crate) trait CloseConnection {
    /// Close the connection
    fn handle_error(&mut self, internal_error: InternalError) -> ConnectionError {
        todo!()
    }
}

pub(crate) trait CloseStream {
    fn handle_error(&mut self, internal_error: InternalError, config: &Config) -> ConnectionError {
        todo!()
    }

    fn close_stream() -> ();

    fn close_connection() -> ();
}
