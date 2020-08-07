use super::varint::VarInt;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ErrorCode(pub(crate) u32);

macro_rules! error_codes {
    {$($name:ident = $val:expr,)*} => {
        impl ErrorCode {
            $(pub const $name: ErrorCode = ErrorCode($val);)*
        }
    }
}

error_codes! {
    NO_ERROR = 0x100,
    GENERAL_PROTOCOL_ERROR = 0x101,
    INTERNAL_ERROR = 0x102,
    STREAM_CREATION_ERROR = 0x103,
    CLOSED_CRITICAL_STREAM = 0x104,
    FRAME_UNEXPECTED = 0x105,
    FRAME_ERROR = 0x106,
    EXCESSIVE_LOAD = 0x107,
    ID_ERROR = 0x108,
    SETTINGS_ERROR = 0x109,
    MISSING_SETTINGS = 0x10A,
    REQUEST_REJECTED = 0x10B,
    REQUEST_CANCELLED = 0x10C,
    REQUEST_INCOMPLETE = 0x10D,
    CONNECT_ERROR = 0x10F,
    VERSION_FALLBACK = 0x110,
    QPACK_DECOMPRESSION_FAILED = 0x200,
    QPACK_ENCODER_STREAM_ERROR = 0x201,
    QPACK_DECODER_STREAM_ERROR = 0x202,
}

impl From<ErrorCode> for VarInt {
    fn from(error: ErrorCode) -> VarInt {
        error.0.into()
    }
}

impl From<VarInt> for ErrorCode {
    fn from(error: VarInt) -> ErrorCode {
        ErrorCode(error.into_inner() as u32)
    }
}
