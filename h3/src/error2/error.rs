//! This is the public facing error types for the h3 crate

use std::sync::Arc;

use crate::quic::ConnectionErrorIncoming;

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

impl ConnectionError {
    /// Create Error for Connection in Closing state
    pub(crate) fn closing() -> Self {
        ConnectionError::Local {
            error: LocalError::Closing,
        }
    }
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
    /// Error is used when violating the MAX_FIELD_SECTION_SIZE
    ///
    /// This can mean different things depending on the context
    /// When sending a request, this means, that the request cannot be sent because the header is larger then permitted by the server
    /// When receiving a request, this means, that the server sent a
    ///
    HeaderTooBig {
        /// The actual size of the header block
        actual_size: u64,
        /// The maximum size of the header block
        max_size: u64,
    },
    /// Undefined error propagated by the quic layer
    Undefined(Arc<dyn std::error::Error + Send + Sync>),
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
            StreamError::RemoteReset { code } => write!(f, "Remote reset: {}", code),
            StreamError::HeaderTooBig {
                actual_size,
                max_size,
            } => write!(
                f,
                "Header too big: actual size: {}, max size: {}",
                actual_size, max_size
            ),
            StreamError::Undefined(err) => write!(f, "Undefined error: {}", err),
        }
    }
}

impl std::error::Error for StreamError {}
