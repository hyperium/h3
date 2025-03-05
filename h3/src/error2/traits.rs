//! Defines error traits

use std::task::{ready, Poll};

use bytes::Buf;

use crate::{
    frame::FrameStreamError,
    quic::{self, ConnectionErrorIncoming, StreamErrorIncoming},
    shared_state::ConnectionState2,
};

use super::{
    codes::NewCode,
    internal_error::{ErrorOrigin, InternalConnectionError},
    ConnectionError, LocalError, StreamError,
};

/// This trait is implemented for all types which can close the connection
pub trait CloseConnection: ConnectionState2 {
    /// Close the connection
    fn handle_connection_error(
        &mut self,
        internal_error: InternalConnectionError,
    ) -> ConnectionError {
        let err = match self.try_set_conn_error(internal_error.clone().into()) {
            Ok(error) => {
                // Ok means set by us so we can close the connection
                self.close_connection(internal_error.code, internal_error.message);
                error
            }
            Err(error) => error,
        };
        // err might be a different error so match again
        match err {
            ErrorOrigin::Internal(internal_error) => {
                ConnectionError::Local {
                    error: LocalError::Application {
                        code: internal_error.code,
                        reason: internal_error.message,
                    },
                }
            }
            ErrorOrigin::Quic(connection_error) => ConnectionError::Remote(connection_error),
        }
    }

    /// Handles an incoming connection error from the quic layer
    fn handle_quic_connection_error(&mut self, error: ConnectionErrorIncoming) -> ConnectionError {
        /*let err = match error {
            ConnectionErrorIncoming::Timeout => ConnectionError::Timeout,
            ConnectionErrorIncoming::InternalError(reason) => ConnectionError::Local {
                error: LocalError::Application {
                    code: NewCode::H3_INTERNAL_ERROR,
                    reason: reason,
                },
            },
            _ => ConnectionError::Remote(error),
        };
        let err = match self.try_set_conn_error(error.into()) {
            Ok(error) => {
                match error {
                    ErrorOrigin::Internal(internal_error) => {
                        ConnectionError::Local {
                            error: LocalError::Application {
                                code: internal_error.code,
                                reason: internal_error.message,
                            },
                        }
                    }
                    ErrorOrigin::Quic(connection_error) => ConnectionError::Remote(connection_error),
                }
            }
            Err(error) => error,
        };
        err*/
        todo!()
    }

    /// Polls for the connection error
    fn poll_connection_error(&mut self, cx: &mut std::task::Context<'_>) -> Poll<ConnectionError> {
        match ready!(self.poll_conn_error(cx)) {
            ErrorOrigin::Internal(internal_error) => {
                Poll::Ready(self.handle_connection_error(internal_error))
            }
            ErrorOrigin::Quic(connection_error) => {
                Poll::Ready(ConnectionError::Remote(connection_error))
            }
        }
    }

    /// Close the connection
    fn close_connection(&mut self, code: NewCode, reason: String) -> ();
}

/// This trait is implemented for all types which can close a stream
pub trait CloseStream: ConnectionState2 {
    /// Handles a connection error on a stream
    fn handle_connection_error_on_stream(
        &mut self,
        internal_error: InternalConnectionError,
    ) -> StreamError {
        /*let error = self.handle_connection_error(internal_error);
        StreamError::ConnectionError(error)*/
        todo!()
    }

    /// Handles a incoming stream error from the quic layer
    fn handle_quic_stream_error(&mut self, error: StreamErrorIncoming) -> StreamError {
        /*match error {
            StreamErrorIncoming::ConnectionErrorIncoming { connection_error } => {
                StreamError::ConnectionError(self.handle_quic_connection_error(connection_error))
            }
            StreamErrorIncoming::StreamReset { error_code } => StreamError::RemoteReset {
                code: NewCode::from(error_code),
            },
            StreamErrorIncoming::Unknown(custom_quic_impl_error) => {
                StreamError::Undefined(custom_quic_impl_error)
            }
        }*/
        todo!()
    }
}

pub(crate) trait CloseRawQuicConnection<B: Buf>: quic::Connection<B> {
    // Should only be used when there is no h3 connection created
    fn handle_quic_error_raw(&mut self, error: ConnectionErrorIncoming) -> ConnectionError {
        match error {
            ConnectionErrorIncoming::Timeout => ConnectionError::Timeout,
            ConnectionErrorIncoming::InternalError(reason) => {
                let local_error = LocalError::Application {
                    code: NewCode::H3_INTERNAL_ERROR,
                    reason: reason.to_string(),
                };
                self.close(NewCode::H3_INTERNAL_ERROR, reason.as_bytes());
                let conn_error = ConnectionError::Local { error: local_error };
                conn_error
            }
            _ => ConnectionError::Remote(error),
        }
    }

    // Should only be used when there is no h3 connection created
    fn close_raw_connection_with_h3_error(
        &mut self,
        internal_error: InternalConnectionError,
    ) -> ConnectionError {
        let error = ConnectionError::Local {
            error: internal_error.clone().into(),
        };
        self.close(internal_error.code, internal_error.message.as_bytes());
        error
    }
}

impl<T, B> CloseRawQuicConnection<B> for T
where
    T: quic::Connection<B>,
    B: Buf,
{
}

pub(crate) trait HandleFrameStreamErrorOnRequestStream {
    fn handle_frame_stream_error_on_request_stream(
        &mut self,
        error: FrameStreamError,
    ) -> StreamError;
}

impl<T> HandleFrameStreamErrorOnRequestStream for T
where
    T: CloseStream,
{
    fn handle_frame_stream_error_on_request_stream(
        &mut self,
        error: FrameStreamError,
    ) -> StreamError {
        match error {
            FrameStreamError::Quic(error) => self.handle_quic_stream_error(error),
            FrameStreamError::Proto(frame_error) => self.handle_connection_error_on_stream(
                InternalConnectionError::got_frame_error(frame_error).into(),
            ),
            FrameStreamError::UnexpectedEnd => {
                self.handle_connection_error_on_stream(InternalConnectionError::new(
                    NewCode::H3_FRAME_ERROR,
                    "received incomplete frame".to_string(),
                ))
            }
        }
    }
}
