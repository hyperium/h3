//! Defines error traits

use tokio::sync::oneshot::error;

use crate::{
    config::Config,
    quic::{ConnectionErrorIncoming, StreamErrorIncoming},
    shared_state::ConnectionState2,
};

use super::{
    codes::NewCode,
    internal_error::{self, ErrorScope, InternalConnectionError, InternalRequestStreamError},
    ConnectionError, LocalError, StreamError,
};

/// This trait is implemented for all types which can close the connection
pub(crate) trait CloseConnection: ConnectionState2 {
    /// Close the connection
    fn handle_connection_error(
        &mut self,
        internal_error: InternalConnectionError,
    ) -> ConnectionError {
        return if let Err(error) = self.get_conn_error() {
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

    /// Handles an incoming connection error from the quic layer
    fn handle_quic_connection_error(&mut self, error: ConnectionErrorIncoming) -> ConnectionError {
        match error {
            ConnectionErrorIncoming::Timeout => ConnectionError::Timeout,
            ConnectionErrorIncoming::InternalError(reason) => {
                if let Err(other_error) = self.get_conn_error() {
                    return other_error;
                }
                let local_error = LocalError::Application {
                    code: NewCode::H3_INTERNAL_ERROR,
                    reason: reason,
                };

                let conn_error = ConnectionError::Local { error: local_error };
                self.set_conn_error(conn_error.clone());
                self.close_connection(&NewCode::H3_INTERNAL_ERROR, reason);
                conn_error
            }
            _ => ConnectionError::Remote(error),
        }
    }

    fn close_connection<T: AsRef<str>>(&mut self, code: &NewCode, reason: T) -> ();
}

pub(crate) trait CloseStream: CloseConnection {
    fn handle_stream_error(&mut self, internal_error: InternalRequestStreamError) -> StreamError {
        return if let Err(error) = self.get_conn_error() {
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

    fn handle_quic_stream_error(&mut self, error: StreamErrorIncoming) -> StreamError {
        match error {
            StreamErrorIncoming::ConnectionErrorIncoming { connection_error } => {
                StreamError::ConnectionError(self.handle_quic_connection_error(connection_error))
            }
            StreamErrorIncoming::StreamReset { error_code } => StreamError::RemoteReset {
                code: NewCode::from(error_code),
            },
        }
    }

    fn close_stream<T: AsRef<str>>(&mut self, code: &NewCode, reason: T) -> ();
}
