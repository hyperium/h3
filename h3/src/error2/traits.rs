//! Defines error traits

use tokio::sync::oneshot::error;

use crate::{config::Config, shared_state::ConnectionState2};

use super::{
    codes::NewCode,
    internal_error::{ErrorScope, InternalConnectionError, InternalRequestStreamError},
    ConnectionError, StreamError,
};

/// This trait is implemented for all types which can close the connection
pub(crate) trait CloseConnection: ConnectionState2 {
    /// Close the connection
    fn handle_connection_error(
        &mut self,
        internal_error: InternalConnectionError,
    ) -> ConnectionError {
        return if let Some(error) = self.get_conn_error() {
            error
        } else {
            let error = ConnectionError::Local {
                error: (&internal_error).into(),
            };
            self.set_conn_error(error.clone());
            self.close_connection(&internal_error.code, internal_error.message);
            error
        };
    }

    fn close_connection<T: AsRef<str>>(&mut self, code: &NewCode, reason: T) -> ();
}

pub(crate) trait CloseStream: ConnectionState2 {
    fn handle_stream_error(
        &mut self,
        internal_error: InternalRequestStreamError,
    ) -> StreamError {
        return if let Some(error) = self.get_conn_error() {
            // If the connection is already in an error state, return the error
            StreamError::ConnectionError(error)
        } else {
            match internal_error.scope {
                ErrorScope::Connection => {
                    // If the error affects the connection, close the connection
                    let conn_error = ConnectionError::Local {
                        error: (&internal_error).into(),
                    };

                    self.set_conn_error(conn_error.clone());
                    let error = StreamError::ConnectionError(conn_error);
                    self.close_connection(&internal_error.code, internal_error.message);
                    error
                }
                ErrorScope::Stream => {
                    // If the error affects the stream, close the stream
                    self.close_stream(&internal_error.code, internal_error.message);

                    let error = StreamError::StreamError {
                        code: internal_error.code,
                        reason: internal_error.message,
                    };
                    error
                }
            }
        };
    }

    fn close_connection<T: AsRef<str>>(&mut self, code: &NewCode, reason: T) -> ();
    fn close_stream<T: AsRef<str>>(&mut self, code: &NewCode, reason: T) -> ();
}
