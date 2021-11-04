use bytes::{Buf, BufMut};
use std::fmt;

use super::coding::{BufExt, BufMutExt, Decode, Encode, UnexpectedEnd};

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct StreamType(u64);

macro_rules! stream_types {
    {$($name:ident = $val:expr,)*} => {
        impl StreamType {
            $(pub const $name: StreamType = StreamType($val);)*
        }
    }
}

stream_types! {
    CONTROL = 0x00,
    PUSH = 0x01,
    ENCODER = 0x02,
    DECODER = 0x03,
}

impl StreamType {
    pub fn value(&self) -> u64 {
        self.0
    }
}

impl Decode for StreamType {
    fn decode<B: Buf>(buf: &mut B) -> Result<Self, UnexpectedEnd> {
        Ok(StreamType(buf.get_var()?))
    }
}

impl Encode for StreamType {
    fn encode<W: BufMut>(&self, buf: &mut W) {
        buf.write_var(self.0);
    }
}

impl fmt::Display for StreamType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            &StreamType::CONTROL => write!(f, "Control"),
            &StreamType::ENCODER => write!(f, "Encoder"),
            &StreamType::DECODER => write!(f, "Decoder"),
            x => write!(f, "StreamType({})", x.0),
        }
    }
}
