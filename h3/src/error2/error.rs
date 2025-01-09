//! This is the public facing error types for the h3 crate

use std::sync::Arc;

use crate::quic;

use super::codes::NewCode;

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
    Remote(Arc<dyn quic::Error>),
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
        reason: &'static str,
    },
    #[non_exhaustive]
    /// The connection is closing
    Closing,
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
        reason: &'static str,
    },
    /// The error occurred on the connection
    #[non_exhaustive]
    ConnectionError(ConnectionError),
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
        }
    }
}

impl std::error::Error for StreamError {}
