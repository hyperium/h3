//! HTTP/3 Error types

use std::{fmt, sync::Arc};

use crate::{
    frame, proto, qpack,
    quic::{self, ConnectionErrorIncoming},
};

/// Error type which faces the user of the library
/// Users of this library will face this error type when a connection error occurs
pub enum H3ConnectionError {
    /// The peer implementation has thrown this error
    PeerError(ConnectionErrorIncoming),
    /// The local implementation has thrown this error
    LocalError,
}

// Warning: this enum is public only for testing purposes. Do not use it in
// downstream code or be prepared to refactor as changes happen.
#[doc(hidden)]
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum ConnectionClosedKind {
    #[non_exhaustive]
    Application {
        code: Code,
        reason: Option<Box<str>>,
        level: ErrorLevel,
    },
    #[non_exhaustive]
    HeaderTooBig {
        actual_size: u64,
        max_size: u64,
    },
    // Error from QUIC layer
    #[non_exhaustive]
    Transport(Arc<TransportError>),
    // Connection has been closed with `Code::NO_ERROR`
    Closed,
    // Currently in a graceful shutdown procedure
    Closing,
    Timeout,
}

// Old Error implementation:
//----------------------------------------------------------------------------------------

/// Cause of an error thrown by our own h3 layer
type Cause = Box<dyn std::error::Error + Send + Sync>;
/// Error thrown by the underlying QUIC impl
pub(crate) type TransportError = Box<dyn quic::Error>;

/// A general error that can occur when handling the HTTP/3 protocol.
#[derive(Clone)]
pub struct LegacyErrorStruct {
    /// The error kind.
    pub(crate) inner: Box<ErrorImpl>,
}

/// An HTTP/3 "application error code".
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
pub struct Code {
    code: u64,
}

impl Code {
    /// Numerical error code
    ///
    /// See <https://www.rfc-editor.org/rfc/rfc9114.html#errors>
    /// and <https://www.rfc-editor.org/rfc/rfc9000.html#error-codes>
    pub fn value(&self) -> u64 {
        self.code
    }
}

impl PartialEq<u64> for Code {
    fn eq(&self, other: &u64) -> bool {
        *other == self.code
    }
}

/// The error kind.
#[derive(Clone)]
pub(crate) struct ErrorImpl {
    pub(crate) kind: LegacyKind,
    cause: Option<Arc<Cause>>,
}

/// Some errors affect the whole connection, others only one Request or Stream.
/// See [errors](https://www.rfc-editor.org/rfc/rfc9114.html#errors) for mor details.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum ErrorLevel {
    /// Error that will close the whole connection
    ConnectionError,
    /// Error scoped to a single stream
    StreamError,
}

// Warning: this enum is public only for testing purposes. Do not use it in
// downstream code or be prepared to refactor as changes happen.
#[doc(hidden)]
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum LegacyKind {
    #[non_exhaustive]
    Application {
        code: Code,
        reason: Option<Box<str>>,
        level: ErrorLevel,
    },
    #[non_exhaustive]
    HeaderTooBig {
        actual_size: u64,
        max_size: u64,
    },
    // Error from QUIC layer
    #[non_exhaustive]
    Transport(Arc<TransportError>),
    // Connection has been closed with `Code::NO_ERROR`
    Closed,
    // Currently in a graceful shutdown procedure
    Closing,
    Timeout,
}

// ===== impl Code =====

macro_rules! codes {
    (
        $(
            $(#[$docs:meta])*
            ($num:expr, $name:ident);
        )+
    ) => {
        impl Code {
        $(
            $(#[$docs])*
            pub const $name: Code = Code{code: $num};
        )+
        }

        impl fmt::Debug for Code {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.code {
                $(
                    $num => f.write_str(stringify!($name)),
                )+
                    other => write!(f, "{:#x}", other),
                }
            }
        }
    }
}

codes! {
    /// Datagram or capsule parse error
    /// See: <https://www.rfc-editor.org/rfc/rfc9297#section-5.2>
    (0x33, H3_DATAGRAM_ERROR);
    /// No error. This is used when the connection or stream needs to be
    /// closed, but there is no error to signal.
    (0x100, H3_NO_ERROR);

    /// Peer violated protocol requirements in a way that does not match a more
    /// specific error code, or endpoint declines to use the more specific
    /// error code.
    (0x101, H3_GENERAL_PROTOCOL_ERROR);

    /// An internal error has occurred in the HTTP stack.
    (0x102, H3_INTERNAL_ERROR);

    /// The endpoint detected that its peer created a stream that it will not
    /// accept.
    (0x103, H3_STREAM_CREATION_ERROR);

    /// A stream required by the HTTP/3 connection was closed or reset.
    (0x104, H3_CLOSED_CRITICAL_STREAM);

    /// A frame was received that was not permitted in the current state or on
    /// the current stream.
    (0x105, H3_FRAME_UNEXPECTED);

    /// A frame that fails to satisfy layout requirements or with an invalid
    /// size was received.
    (0x106, H3_FRAME_ERROR);

    /// The endpoint detected that its peer is exhibiting a behavior that might
    /// be generating excessive load.
    (0x107, H3_EXCESSIVE_LOAD);

    /// A Stream ID or Push ID was used incorrectly, such as exceeding a limit,
    /// reducing a limit, or being reused.
    (0x108, H3_ID_ERROR);

    /// An endpoint detected an error in the payload of a SETTINGS frame.
    (0x109, H3_SETTINGS_ERROR);

    /// No SETTINGS frame was received at the beginning of the control stream.
    (0x10a, H3_MISSING_SETTINGS);

    /// A server rejected a request without performing any application
    /// processing.
    (0x10b, H3_REQUEST_REJECTED);

    /// The request or its response (including pushed response) is cancelled.
    (0x10c, H3_REQUEST_CANCELLED);

    /// The client's stream terminated without containing a fully-formed
    /// request.
    (0x10d, H3_REQUEST_INCOMPLETE);

    /// An HTTP message was malformed and cannot be processed.
    (0x10e, H3_MESSAGE_ERROR);

    /// The TCP connection established in response to a CONNECT request was
    /// reset or abnormally closed.
    (0x10f, H3_CONNECT_ERROR);

    /// The requested operation cannot be served over HTTP/3. The peer should
    /// retry over HTTP/1.1.
    (0x110, H3_VERSION_FALLBACK);

    /// The decoder failed to interpret an encoded field section and is not
    /// able to continue decoding that field section.
    (0x200, QPACK_DECOMPRESSION_FAILED);

    /// The decoder failed to interpret an encoder instruction received on the
    /// encoder stream.
    (0x201, QPACK_ENCODER_STREAM_ERROR);

    /// The encoder failed to interpret a decoder instruction received on the
    /// decoder stream.
    (0x202, QPACK_DECODER_STREAM_ERROR);
}

impl Code {
    pub(crate) fn with_reason<S: Into<Box<str>>>(
        self,
        reason: S,
        level: ErrorLevel,
    ) -> LegacyErrorStruct {
        LegacyErrorStruct::new(LegacyKind::Application {
            code: self,
            reason: Some(reason.into()),
            level,
        })
    }

    pub(crate) fn with_cause<E: Into<Cause>>(self, cause: E) -> LegacyErrorStruct {
        LegacyErrorStruct::from(self).with_cause(cause)
    }

    pub(crate) fn with_transport<E: Into<Box<dyn quic::Error>>>(self, err: E) -> LegacyErrorStruct {
        LegacyErrorStruct::new(LegacyKind::Transport(Arc::new(err.into())))
    }
}

impl From<Code> for u64 {
    fn from(code: Code) -> u64 {
        code.code
    }
}

// ===== impl Error =====

impl LegacyErrorStruct {
    fn new(kind: LegacyKind) -> Self {
        LegacyErrorStruct {
            inner: Box::new(ErrorImpl { kind, cause: None }),
        }
    }

    /// Returns the error code from the error if available
    pub fn try_get_code(&self) -> Option<Code> {
        match self.inner.kind {
            LegacyKind::Application { code, .. } => Some(code),
            _ => None,
        }
    }

    /// returns the [`ErrorLevel`] of an [`Error`]
    /// This indicates weather a accept loop should continue.
    pub fn get_error_level(&self) -> ErrorLevel {
        match self.inner.kind {
            LegacyKind::Application {
                code: _,
                reason: _,
                level,
            } => level,
            // return Connection error on other kinds
            _ => ErrorLevel::ConnectionError,
        }
    }

    pub(crate) fn header_too_big(actual_size: u64, max_size: u64) -> Self {
        LegacyErrorStruct::new(LegacyKind::HeaderTooBig {
            actual_size,
            max_size,
        })
    }

    pub(crate) fn with_cause<E: Into<Cause>>(mut self, cause: E) -> Self {
        self.inner.cause = Some(Arc::new(cause.into()));
        self
    }

    pub(crate) fn closing() -> Self {
        Self::new(LegacyKind::Closing)
    }

    pub(crate) fn closed() -> Self {
        Self::new(LegacyKind::Closed)
    }

    pub(crate) fn is_closed(&self) -> bool {
        if let LegacyKind::Closed = self.inner.kind {
            return true;
        }
        false
    }

    pub(crate) fn is_header_too_big(&self) -> bool {
        matches!(&self.inner.kind, LegacyKind::HeaderTooBig { .. })
    }

    #[doc(hidden)]
    pub fn kind(&self) -> LegacyKind {
        self.inner.kind.clone()
    }
}

impl fmt::Debug for LegacyErrorStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = f.debug_struct("h3::Error");

        match self.inner.kind {
            LegacyKind::Closed => {
                builder.field("connection closed", &true);
            }
            LegacyKind::Closing => {
                builder.field("closing", &true);
            }
            LegacyKind::Timeout => {
                builder.field("timeout", &true);
            }
            LegacyKind::Application {
                code, ref reason, ..
            } => {
                builder.field("code", &code);
                if let Some(reason) = reason {
                    builder.field("reason", reason);
                }
            }
            LegacyKind::Transport(ref e) => {
                builder.field("kind", &e);
                builder.field("code: ", &e.err_code());
            }
            LegacyKind::HeaderTooBig {
                actual_size,
                max_size,
            } => {
                builder.field("header_size", &actual_size);
                builder.field("max_size", &max_size);
            }
        }

        if let Some(ref cause) = self.inner.cause {
            builder.field("cause", cause);
        }

        builder.finish()
    }
}

impl fmt::Display for LegacyErrorStruct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.inner.kind {
            LegacyKind::Closed => write!(f, "connection is closed")?,
            LegacyKind::Closing => write!(f, "connection is gracefully closing")?,
            LegacyKind::Transport(ref e) => write!(f, "quic transport error: {}", e)?,
            LegacyKind::Timeout => write!(f, "timeout",)?,
            LegacyKind::Application {
                code, ref reason, ..
            } => {
                if let Some(reason) = reason {
                    write!(f, "application error: {}", reason)?
                } else {
                    write!(f, "application error {:?}", code)?
                }
            }
            LegacyKind::HeaderTooBig {
                actual_size,
                max_size,
            } => write!(
                f,
                "issued header size {} o is beyond peer's limit {} o",
                actual_size, max_size
            )?,
        };
        if let Some(ref cause) = self.inner.cause {
            write!(f, "cause: {}", cause)?
        }
        Ok(())
    }
}

impl std::error::Error for LegacyErrorStruct {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.cause.as_ref().map(|e| &***e as _)
    }
}

impl From<Code> for LegacyErrorStruct {
    fn from(code: Code) -> LegacyErrorStruct {
        LegacyErrorStruct::new(LegacyKind::Application {
            code,
            reason: None,
            level: ErrorLevel::ConnectionError,
        })
    }
}

impl From<qpack::EncoderError> for LegacyErrorStruct {
    fn from(e: qpack::EncoderError) -> Self {
        Self::from(Code::QPACK_ENCODER_STREAM_ERROR).with_cause(e)
    }
}

impl From<qpack::DecoderError> for LegacyErrorStruct {
    fn from(e: qpack::DecoderError) -> Self {
        match e {
            qpack::DecoderError::InvalidStaticIndex(_) => {
                Self::from(Code::QPACK_DECOMPRESSION_FAILED).with_cause(e)
            }
            _ => Self::from(Code::QPACK_DECODER_STREAM_ERROR).with_cause(e),
        }
    }
}

impl From<proto::headers::HeaderError> for LegacyErrorStruct {
    fn from(e: proto::headers::HeaderError) -> Self {
        LegacyErrorStruct::new(LegacyKind::Application {
            code: Code::H3_MESSAGE_ERROR,
            reason: None,
            level: ErrorLevel::StreamError,
        })
        .with_cause(e)
    }
}

impl From<frame::FrameStreamError> for LegacyErrorStruct {
    fn from(e: frame::FrameStreamError) -> Self {
        match e {
            frame::FrameStreamError::Quic(e) => e.into(),

            //= https://www.rfc-editor.org/rfc/rfc9114#section-7.1
            //# When a stream terminates cleanly, if the last frame on the stream was
            //# truncated, this MUST be treated as a connection error of type
            //# H3_FRAME_ERROR.
            frame::FrameStreamError::UnexpectedEnd => Code::H3_FRAME_ERROR
                .with_reason("received incomplete frame", ErrorLevel::ConnectionError),

            frame::FrameStreamError::Proto(e) => match e {
                proto::frame::FrameError::InvalidStreamId(_)
                | proto::frame::FrameError::InvalidPushId(_) => Code::H3_ID_ERROR,
                proto::frame::FrameError::Settings(_) => Code::H3_SETTINGS_ERROR,
                proto::frame::FrameError::UnsupportedFrame(_)
                | proto::frame::FrameError::UnknownFrame(_) => Code::H3_FRAME_UNEXPECTED,

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.1
                //# A frame payload that contains additional bytes
                //# after the identified fields or a frame payload that terminates before
                //# the end of the identified fields MUST be treated as a connection
                //# error of type H3_FRAME_ERROR.

                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.1
                //# In particular, redundant length
                //# encodings MUST be verified to be self-consistent; see Section 10.8.
                proto::frame::FrameError::Incomplete(_)
                | proto::frame::FrameError::InvalidFrameValue
                | proto::frame::FrameError::Malformed => Code::H3_FRAME_ERROR,
            }
            .with_cause(e),
        }
    }
}

impl From<LegacyErrorStruct> for Box<dyn std::error::Error + std::marker::Send> {
    fn from(e: LegacyErrorStruct) -> Self {
        Box::new(e)
    }
}

impl<T> From<T> for LegacyErrorStruct
where
    T: Into<TransportError>,
{
    fn from(e: T) -> Self {
        let quic_error: TransportError = e.into();
        if quic_error.is_timeout() {
            return LegacyErrorStruct::new(LegacyKind::Timeout);
        }

        match quic_error.err_code() {
            Some(c) if Code::H3_NO_ERROR == c => LegacyErrorStruct::new(LegacyKind::Closed),
            Some(c) => LegacyErrorStruct::new(LegacyKind::Application {
                code: Code { code: c },
                reason: None,
                level: ErrorLevel::ConnectionError,
            }),
            None => LegacyErrorStruct::new(LegacyKind::Transport(Arc::new(quic_error))),
        }
    }
}

impl From<proto::stream::InvalidStreamId> for LegacyErrorStruct {
    fn from(e: proto::stream::InvalidStreamId) -> Self {
        Self::from(Code::H3_ID_ERROR).with_cause(format!("{}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::LegacyErrorStruct;
    use std::mem;

    #[test]
    fn test_size_of() {
        assert_eq!(mem::size_of::<LegacyErrorStruct>(), mem::size_of::<usize>());
    }
}
