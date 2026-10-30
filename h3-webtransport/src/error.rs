//! Application error codes for WebTransport data streams.
//!
//! These mappings apply to RESET_STREAM and STOP_SENDING, not to the code in a
//! session close capsule. The underlying `h3::quic` traits still use wire codes.

use std::{fmt, io};

use h3::quic::StreamErrorIncoming;

const FIRST: u64 = 0x52e4a40fa8db;
const LAST: u64 = 0x52e5ac983162;

/// Map a 32-bit application code into the HTTP/3 WebTransport error range.
///
/// Reserved HTTP/3 codepoints of the form `0x1f * N + 0x21` are skipped.
/// See <https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3-16#section-4.4>.
pub const fn to_http3_code(code: u32) -> u64 {
    let code = code as u64;
    FIRST + code + code / 0x1e
}

/// Decode an HTTP/3 stream error code into its WebTransport application code.
///
/// Returns `None` for reserved codepoints and codes outside the application
/// error range, including WebTransport protocol errors such as WT_SESSION_GONE.
pub const fn from_http3_code(code: u64) -> Option<u32> {
    if code < FIRST || code > LAST || code % 0x1f == 0x21 % 0x1f {
        return None;
    }
    let shifted = code - FIRST;
    Some((shifted - shifted / 0x1f) as u32)
}

/// A WebTransport data-stream failure, retaining its underlying transport error.
///
/// Tokio and futures I/O errors carry this type as their inner error. Use
/// [`io::Error::get_ref`] and `downcast_ref::<StreamError>()` to inspect it.
#[derive(Debug)]
pub struct StreamError {
    inner: StreamErrorIncoming,
}

impl StreamError {
    /// The peer's application code for a stream reset or STOP_SENDING.
    ///
    /// Protocol codes, reserved values, connection failures, and unknown
    /// transport failures have no application code.
    pub fn application_code(&self) -> Option<u32> {
        self.http3_code().and_then(from_http3_code)
    }

    /// The original wire code for a stream reset or STOP_SENDING, if present.
    pub fn http3_code(&self) -> Option<u64> {
        match &self.inner {
            StreamErrorIncoming::StreamTerminated { error_code } => Some(*error_code),
            _ => None,
        }
    }

    /// The original transport failure, without changing its code or scope.
    pub fn transport_error(&self) -> &StreamErrorIncoming {
        &self.inner
    }
}

impl From<StreamErrorIncoming> for StreamError {
    fn from(inner: StreamErrorIncoming) -> Self {
        Self { inner }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.application_code() {
            Some(code) => write!(
                f,
                "WebTransport stream terminated with application code {code}"
            ),
            None => fmt::Display::fmt(&self.inner, f),
        }
    }
}

impl std::error::Error for StreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

pub(crate) fn into_io_error(error: StreamErrorIncoming) -> io::Error {
    io::Error::other(StreamError::from(error))
}

pub(crate) fn map_io_error(error: io::Error) -> io::Error {
    if !error
        .get_ref()
        .is_some_and(|inner| inner.is::<StreamErrorIncoming>())
    {
        return error;
    }
    let kind = error.kind();
    let inner = error
        .into_inner()
        .unwrap()
        .downcast::<StreamErrorIncoming>()
        .unwrap();
    io::Error::new(kind, StreamError::from(*inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use h3::quic::ConnectionErrorIncoming;

    #[test]
    fn boundaries_and_reserved_codepoints_follow_the_wire_mapping() {
        assert_eq!(to_http3_code(0), FIRST);
        assert_eq!(to_http3_code(u32::MAX), LAST);
        assert_eq!(to_http3_code(29), FIRST + 29);
        assert_eq!(to_http3_code(30), FIRST + 31);
        assert_eq!(from_http3_code(FIRST + 30), None);
        for code in [0, 1, 28, 29, 30, 31, 59, 60, u32::MAX - 1, u32::MAX] {
            let wire = to_http3_code(code);
            assert_ne!((wire - 0x21) % 0x1f, 0);
            assert_eq!(from_http3_code(wire), Some(code));
        }
        for wire in [0, 0x100, 0x170d7b68, FIRST - 1, LAST + 1, u64::MAX] {
            assert_eq!(from_http3_code(wire), None);
        }
    }

    #[test]
    fn mapping_is_bijective_near_both_ends_and_skips_every_reserved_value() {
        for code in (0..4096).chain((u32::MAX - 4096)..=u32::MAX) {
            let wire = to_http3_code(code);
            assert_eq!(from_http3_code(wire), Some(code));
            if code != u32::MAX {
                let next = to_http3_code(code + 1);
                assert!(next > wire);
                if next > wire + 1 {
                    assert_eq!(from_http3_code(wire + 1), None);
                }
            }
        }
    }

    #[test]
    fn transport_scope_and_unmapped_codes_are_preserved() {
        for wire in [FIRST + 30, FIRST - 1, LAST + 1, 0x170d7b68] {
            let error =
                StreamError::from(StreamErrorIncoming::StreamTerminated { error_code: wire });
            assert_eq!(error.application_code(), None);
            assert_eq!(error.http3_code(), Some(wire));
        }
        let error = StreamError::from(StreamErrorIncoming::ConnectionErrorIncoming {
            connection_error: ConnectionErrorIncoming::ApplicationClose { error_code: FIRST },
        });
        assert_eq!(error.application_code(), None);
        assert_eq!(error.http3_code(), None);
        assert!(matches!(
            error.transport_error(),
            StreamErrorIncoming::ConnectionErrorIncoming { .. }
        ));
    }

    #[test]
    fn io_wrapping_keeps_kind_cause_and_application_code() {
        let error = io::Error::new(
            io::ErrorKind::ConnectionReset,
            StreamErrorIncoming::StreamTerminated {
                error_code: to_http3_code(42),
            },
        );
        let error = map_io_error(error);
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        let inner = error
            .get_ref()
            .unwrap()
            .downcast_ref::<StreamError>()
            .unwrap();
        assert_eq!(inner.application_code(), Some(42));
        assert_eq!(inner.http3_code(), Some(to_http3_code(42)));
        let error = map_io_error(error);
        assert!(error.get_ref().unwrap().is::<StreamError>());
        let other = map_io_error(io::Error::new(io::ErrorKind::BrokenPipe, "other failure"));
        assert_eq!(other.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(other.to_string(), "other failure");
    }
}
