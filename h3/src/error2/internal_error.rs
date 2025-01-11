use crate::error::Code;

/// This enum determines if the error affects the connection or the stream
///
/// The end goal is to make all stream errors from the spec potential connection errors if users of h3 decide to treat them as such
#[derive(Debug, Clone, Hash)]
pub enum ErrorScope {
    /// The error affects the connection
    Connection,
    /// The error affects the stream
    Stream,
}

/// This error type represents an internal error type, which is used
/// to represent errors, which have not yet affected the connection or stream state
///
/// This error type is generated from the error types of h3s submodules or by the modules itself.
#[derive(Debug, Clone, Hash)]
pub struct InternalError {
    /// The error scope
    scope: ErrorScope,
    /// The error code
    code: Code,
    /// The error message
    message: &'static str,
}