use crate::frame::FrameProtocolError;

use super::codes::NewCode;

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
/// to represent errors, which have not yet affected the connection state
///
/// This error type is generated from the error types of h3s submodules or by the modules itself.
///
/// This error type is used in functions which handle a http3 connection state
#[derive(Debug, Clone, Hash)]
pub struct InternalConnectionError {
    /// The error code
    pub(super) code: NewCode,
    /// The error message
    pub(super) message: String,
}

impl InternalConnectionError {
    /// Create a new internal connection error
    pub fn new(code: NewCode, message: String) -> Self {
        Self { code, message }
    }
        /// Creates a new internal connection error from a frame error
        pub fn got_frame_error(value: FrameProtocolError) -> Self {
            match value {
                FrameProtocolError::InvalidStreamId(id) => InternalConnectionError {
                    code: NewCode::H3_ID_ERROR,
                    message: format!("invalid stream id: {}", id),
                },
                FrameProtocolError::InvalidPushId(id) => InternalConnectionError {
                    code: NewCode::H3_ID_ERROR,
                    message: format!("invalid push id: {}", id),
                },
                FrameProtocolError::Settings(error) => InternalConnectionError {
                    // TODO: Check spec which error code to return when a bad settings frame arrives on a stream which is not allowed to have settings
                    //       At the moment, because the Frame is parsed before the stream type is checked, the H3_SETTINGS_ERROR is returned.
                    //       Same for the InvalidStreamId and InvalidPushId
                        code: NewCode::H3_SETTINGS_ERROR,
                        message: error.to_string(),
                },
                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.8
                //# These frame
                //# types MUST NOT be sent, and their receipt MUST be treated as a
                //# connection error of type H3_FRAME_UNEXPECTED.
                FrameProtocolError::ForbiddenFrame(number) => InternalConnectionError {
                        code: NewCode::H3_FRAME_UNEXPECTED,
                        message: format!("received a forbidden frame with number {}", number),
                },
                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.1
                //# A frame payload that contains additional bytes
                //# after the identified fields or a frame payload that terminates before
                //# the end of the identified fields MUST be treated as a connection
                //# error of type H3_FRAME_ERROR.
                FrameProtocolError::InvalidFrameValue | FrameProtocolError::Malformed => InternalConnectionError {
                    code: NewCode::H3_FRAME_ERROR,
                    message: "frame payload that contains additional bytes after the identified fields or a frame payload that terminates before the end of the identified fields".to_string(),
                },        
            }
        }    
}