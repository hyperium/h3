//! This is the public facing error types for the h3 crate

use std::sync::Arc;

use crate::quic::{self, ConnectionErrorIncoming, StreamErrorIncoming};

use super::{codes::NewCode, internal_error::InternalConnectionError};

/// This enum represents wether the error occurred on the local or remote side of the connection
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ConnectionError {
    /// The error occurred on the local side of the connection
    #[non_exhaustive]
    Local {
        /// The error
        error: LocalError,
    },
    /// Error returned by the quic layer
    /// I might be an quic error or the remote h3 connection closed the connection with an error
    #[non_exhaustive]
    Remote(ConnectionErrorIncoming),
    /// Timeout occurred
    #[non_exhaustive]
    Timeout,
}

/// This enum represents a local error
#[derive(Debug, Clone, Hash)]
#[non_exhaustive]
pub enum LocalError {
    #[non_exhaustive]
    /// The application closed the connection
    Application {
        /// The error code
        code: NewCode,
        /// The error reason
        reason: String,
    },
    #[non_exhaustive]
    /// The connection is closing
    Closing,
}

impl From<InternalConnectionError> for LocalError {
    fn from(err: InternalConnectionError) -> Self {
        LocalError::Application {
            code: err.code,
            reason: err.message,
        }
    }
}

/// This enum represents a stream error
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamError {
    /// The error occurred on the stream
    #[non_exhaustive]
    StreamError {
        /// The error code
        code: NewCode,
        /// The error reason
        reason: String,
    },
    /// Stream was Reset by the peer
    RemoteReset {
        /// Reset code received from the peer
        code: NewCode,
    },
    /// The error occurred on the connection
    #[non_exhaustive]
    ConnectionError(ConnectionError),
}

/// This enum represents a stream error
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ServerStreamError {
    /// A Stream error occurred
    General(StreamError),
    #[non_exhaustive]
    /// The received header block is too big
    /// The Request has been answered with a 431 Request Header Fields Too Large
    HeaderTooBig {
        /// The actual size of the header block
        actual_size: u64,
        /// The maximum size of the header block
        max_size: u64,
    },
}

#[derive(Debug, Clone)]
#[non_exhaustive]
/// This enum represents a stream error
///
/// This type will be returned by the client in case of an error on the stream methods
pub enum ClientStreamError {
    /// A Stream error occurred
    General(StreamError),
    #[non_exhaustive]
    /// The request cannot be sent because the header block larger then permitted by the server
    HeaderTooBig {
        /// The actual size of the header block
        actual_size: u64,
        /// The maximum size of the header block
        max_size: u64,
    },
}

impl std::fmt::Display for ClientStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientStreamError::General(stream_error) => {
                write!(f, "Stream error: {}", stream_error)
            }
            ClientStreamError::HeaderTooBig {
                actual_size,
                max_size,
            } => write!(
                f,
                "Request cannot be sent because the header is lager than permitted by the server: permitted size: {}, max size: {}",
                actual_size, max_size
            ),
        }
    }
}

impl std::fmt::Display for ServerStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerStreamError::General(stream_error) => {
                write!(f, "Stream error: {}", stream_error)
            }
            ServerStreamError::HeaderTooBig {
                actual_size,
                max_size,
            } => write!(
                f,
                "Header too big: actual size: {}, max size: {}",
                actual_size, max_size
            ),
        }
    }
}

impl std::error::Error for ServerStreamError {}

impl From<StreamError> for ServerStreamError {
    fn from(err: StreamError) -> Self {
        ServerStreamError::General(err)
    }
}

impl From<StreamError> for ClientStreamError {
    fn from(err: StreamError) -> Self {
        ClientStreamError::General(err)
    }
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::Local { error } => write!(f, "Local error: {:?}", error),
            ConnectionError::Remote(err) => write!(f, "Remote error: {:?}", err),
            ConnectionError::Timeout => write!(f, "Timeout"),
        }
    }
}

impl std::error::Error for ConnectionError {}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StreamError::StreamError { code, reason } => {
                write!(f, "Stream error: {:?} - {}", code, reason)
            }
            StreamError::ConnectionError(err) => write!(f, "Connection error: {}", err),
            StreamError::RemoteReset { code } => write!(f, "Remote reset: {:?}", code),
        }
    }
}

impl std::error::Error for StreamError {}
