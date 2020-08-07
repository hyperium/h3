use std::fmt;

use bytes::{buf::ext::BufExt as _, Buf, BufMut, Bytes};
use tracing::trace;

use super::varint::{BufExt, BufMutExt, UnexpectedEnd, VarInt};

#[derive(Debug, PartialEq)]
pub enum Error {
    Malformed,
    UnsupportedFrame(u64), // Known frames that should generate an error
    UnknownFrame(u64),     // Unknown frames that should be ignored
    UnexpectedEnd,
    InvalidFrameValue,
    InvalidSettingId(u64),
    InvalidSettingValue(SettingId, u64),
    SettingRepeated(SettingId),
    SettingsExceeded,
    Incomplete(usize),
    IncompleteData,
    Settings(String),
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    Data(DataFrame<Bytes>),
    Headers(HeadersFrame),
    CancelPush(u64),
    Settings(SettingsFrame),
    PushPromise(PushPromiseFrame),
    Goaway(u64),
    MaxPushId(u64),
    DuplicatePush(u64),
    Reserved,
}

impl Frame {
    pub fn encode<T: BufMut>(&self, buf: &mut T) {
        match self {
            Frame::Data(f) => f.encode(buf),
            Frame::Headers(f) => f.encode(buf),
            Frame::Settings(f) => f.encode(buf),
            Frame::CancelPush(id) => simple_frame_encode(FrameType::CANCEL_PUSH, *id, buf),
            Frame::PushPromise(f) => f.encode(buf),
            Frame::Goaway(id) => simple_frame_encode(FrameType::GOAWAY, *id, buf),
            Frame::MaxPushId(id) => simple_frame_encode(FrameType::MAX_PUSH_ID, *id, buf),
            Frame::DuplicatePush(id) => simple_frame_encode(FrameType::DUPLICATE_PUSH, *id, buf),
            Frame::Reserved => (),
        }
    }

    pub fn decode<T: Buf>(buf: &mut T) -> Result<Self, Error> {
        let remaining = buf.remaining();
        let ty = FrameType::decode(buf).map_err(|_| Error::Incomplete(remaining + 1 as usize))?;
        let len = buf
            .get_var()
            .map_err(|_| Error::Incomplete(remaining + 1 as usize))?;

        if buf.remaining() < len as usize {
            if ty == FrameType::DATA {
                return Err(Error::IncompleteData);
            }
            return Err(Error::Incomplete(2 + len as usize));
        }

        let mut payload = buf.take(len as usize);
        let frame = match ty {
            FrameType::DATA => Ok(Frame::Data(DataFrame {
                payload: payload.to_bytes(),
            })),
            FrameType::HEADERS => Ok(Frame::Headers(HeadersFrame::decode(&mut payload)?)),
            FrameType::SETTINGS => Ok(Frame::Settings(SettingsFrame::decode(&mut payload)?)),
            FrameType::CANCEL_PUSH => Ok(Frame::CancelPush(payload.get_var()?)),
            FrameType::PUSH_PROMISE => {
                Ok(Frame::PushPromise(PushPromiseFrame::decode(&mut payload)?))
            }
            FrameType::GOAWAY => Ok(Frame::Goaway(payload.get_var()?)),
            FrameType::MAX_PUSH_ID => Ok(Frame::MaxPushId(payload.get_var()?)),
            FrameType::DUPLICATE_PUSH => Ok(Frame::DuplicatePush(payload.get_var()?)),
            FrameType::H2_PRIORITY
            | FrameType::H2_PING
            | FrameType::H2_WINDOW_UPDATE
            | FrameType::H2_CONTINUATION => Err(Error::UnsupportedFrame(ty.0)),
            t if t.0 > 0x21 && (t.0 - 0x21) % 0x1f == 0 => {
                buf.advance(len as usize);
                Ok(Frame::Reserved)
            }
            _ => {
                buf.advance(len as usize);
                Err(Error::UnknownFrame(ty.0))
            }
        };
        if let Ok(frame) = &frame {
            trace!(
                "got frame {}, len: {}, remaining: {}",
                frame,
                len,
                buf.remaining()
            );
        }
        frame
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Frame::Data(frame) => write!(f, "Data({} bytes)", frame.len()),
            Frame::Headers(frame) => write!(f, "Headers({} entries)", frame.len()),
            Frame::Settings(_) => write!(f, "Settings"),
            Frame::CancelPush(id) => write!(f, "CancelPush({})", id),
            Frame::PushPromise(frame) => write!(f, "PushPromise({})", frame.id),
            Frame::Goaway(id) => write!(f, "GoAway({})", id),
            Frame::MaxPushId(id) => write!(f, "MaxPushId({})", id),
            Frame::DuplicatePush(id) => write!(f, "DuplicatePush({})", id),
            Frame::Reserved => write!(f, "Reserved"),
        }
    }
}
macro_rules! frame_types {
    {$($name:ident = $val:expr,)*} => {
        impl FrameType {
            $(pub const $name: FrameType = FrameType($val);)*
        }
    }
}

frame_types! {
    DATA = 0x0,
    HEADERS = 0x1,
    H2_PRIORITY = 0x2,
    CANCEL_PUSH = 0x3,
    SETTINGS = 0x4,
    PUSH_PROMISE = 0x5,
    H2_PING = 0x6,
    GOAWAY = 0x7,
    H2_WINDOW_UPDATE = 0x8,
    H2_CONTINUATION = 0x9,
    MAX_PUSH_ID = 0xD,
    DUPLICATE_PUSH = 0xE,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(crate) struct FrameType(u64);

impl FrameType {
    fn decode<B: Buf>(buf: &mut B) -> Result<Self, UnexpectedEnd> {
        Ok(FrameType(buf.get_var()?))
    }
    fn encode<B: BufMut>(&self, buf: &mut B) {
        buf.write_var(self.0);
    }
}

pub(crate) trait FrameHeader {
    fn len(&self) -> usize;
    const TYPE: FrameType;
    fn encode_header<T: BufMut>(&self, buf: &mut T) {
        Self::TYPE.encode(buf);
        buf.write_var(self.len() as u64);
    }
}

pub(crate) trait IntoPayload {
    fn into_payload(&mut self) -> &mut dyn Buf;
}

#[derive(Debug, PartialEq)]
pub struct DataFrame<P> {
    pub payload: P,
}

impl<P> DataFrame<P>
where
    P: Buf,
{
    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        self.encode_header(buf);
        buf.put(self.payload.bytes());
    }
}

impl<P> FrameHeader for DataFrame<P>
where
    P: Buf,
{
    const TYPE: FrameType = FrameType::DATA;
    fn len(&self) -> usize {
        self.payload.remaining()
    }
}

impl<P> IntoPayload for DataFrame<P>
where
    P: Buf,
{
    fn into_payload(&mut self) -> &mut dyn Buf {
        &mut self.payload
    }
}

pub struct PartialData {
    remaining: usize,
}

impl PartialData {
    pub fn decode<B: Buf>(buf: &mut B) -> Result<(Self, DataFrame<Bytes>), Error> {
        if FrameType::DATA != FrameType::decode(buf)? {
            panic!("can only decode Data frames");
        }

        let len = buf.get_var()? as usize;
        let payload = buf.to_bytes();

        Ok((
            Self {
                remaining: len - payload.len(),
            },
            DataFrame { payload },
        ))
    }

    pub fn decode_data<B: Buf>(&mut self, buf: &mut B) -> DataFrame<Bytes> {
        let payload = buf.take(self.remaining).to_bytes();
        self.remaining -= payload.len();
        DataFrame { payload }
    }

    pub fn remaining(&self) -> usize {
        self.remaining
    }
}

#[derive(Debug, PartialEq)]
pub struct HeadersFrame {
    pub encoded: Bytes,
}

impl FrameHeader for HeadersFrame {
    const TYPE: FrameType = FrameType::HEADERS;
    fn len(&self) -> usize {
        self.encoded.as_ref().len()
    }
}

impl HeadersFrame {
    fn decode<B: Buf>(buf: &mut B) -> Result<Self, UnexpectedEnd> {
        Ok(HeadersFrame {
            encoded: buf.to_bytes(),
        })
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        self.encode_header(buf);
        buf.put(self.encoded.clone());
    }
}

impl IntoPayload for HeadersFrame {
    fn into_payload(&mut self) -> &mut dyn Buf {
        &mut self.encoded
    }
}

#[derive(Debug, PartialEq)]
pub struct PushPromiseFrame {
    id: u64,
    encoded: Bytes,
}

impl FrameHeader for PushPromiseFrame {
    const TYPE: FrameType = FrameType::PUSH_PROMISE;
    fn len(&self) -> usize {
        VarInt::from_u64(self.id).unwrap().size() + self.encoded.as_ref().len()
    }
}

impl PushPromiseFrame {
    fn decode<B: Buf>(buf: &mut B) -> Result<Self, UnexpectedEnd> {
        Ok(PushPromiseFrame {
            id: buf.get_var()?,
            encoded: buf.to_bytes(),
        })
    }
    fn encode<B: BufMut>(&self, buf: &mut B) {
        self.encode_header(buf);
        buf.write_var(self.id);
        buf.put(self.encoded.clone());
    }
}

fn simple_frame_encode<B: BufMut>(ty: FrameType, id: u64, buf: &mut B) {
    ty.encode(buf);
    buf.write_var(1);
    buf.write_var(id);
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct SettingId(pub u64);

impl SettingId {
    const NONE: SettingId = SettingId(0);

    fn is_supported(self) -> bool {
        match self {
            SettingId::MAX_HEADER_LIST_SIZE
            | SettingId::QPACK_MAX_TABLE_CAPACITY
            | SettingId::QPACK_MAX_BLOCKED_STREAMS => true,
            _ => false,
        }
    }

    fn decode<B: Buf>(buf: &mut B) -> Result<Self, UnexpectedEnd> {
        Ok(SettingId(buf.get_var()?))
    }

    fn encode<B: BufMut>(&self, buf: &mut B) {
        buf.write_var(self.0);
    }
}

macro_rules! setting_identifiers {
    {$($name:ident = $val:expr,)*} => {
        impl SettingId {
            $(pub const $name: SettingId = SettingId($val);)*
        }
    }
}

setting_identifiers! {
    QPACK_MAX_TABLE_CAPACITY = 0x1,
    QPACK_MAX_BLOCKED_STREAMS = 0x7,
    MAX_HEADER_LIST_SIZE = 0x6,
}

#[derive(Debug, PartialEq)]
pub struct SettingsFrame {
    entries: [(SettingId, u64); 3],
    len: usize,
}

impl Default for SettingsFrame {
    fn default() -> Self {
        Self {
            entries: [(SettingId::NONE, 0); 3],
            len: 0,
        }
    }
}

impl FrameHeader for SettingsFrame {
    const TYPE: FrameType = FrameType::SETTINGS;
    fn len(&self) -> usize {
        self.entries[..self.len].iter().fold(0, |len, (id, val)| {
            len + VarInt::from_u64(id.0).unwrap().size() + VarInt::from_u64(*val).unwrap().size()
        })
    }
}

impl SettingsFrame {
    fn insert(&mut self, id: SettingId, value: u64) -> Result<(), SettingsError> {
        if self.len >= self.entries.len() {
            return Err(SettingsError::Exceeded);
        }

        if !id.is_supported() {
            return Ok(());
        }

        if self.entries[..self.len].iter().any(|(i, _)| *i == id) {
            return Err(SettingsError::Repeated(id));
        }

        self.entries[self.len] = (id, value);
        self.len += 1;
        Ok(())
    }

    pub(super) fn encode<T: BufMut>(&self, buf: &mut T) {
        self.encode_header(buf);
        for (id, val) in self.entries[..self.len].iter() {
            id.encode(buf);
            buf.write_var(*val);
        }
    }

    pub(super) fn decode<T: Buf>(buf: &mut T) -> Result<SettingsFrame, SettingsError> {
        let mut settings = SettingsFrame::default();
        while buf.has_remaining() {
            if buf.remaining() < 2 {
                // remains less than 2 * minimum-size varint
                return Err(SettingsError::Malformed);
            }

            let identifier = SettingId::decode(buf).map_err(|_| SettingsError::Malformed)?;
            let value = buf.get_var().map_err(|_| SettingsError::Malformed)?;

            if identifier.is_supported() {
                settings.insert(identifier, value)?;
            }
        }
        Ok(settings)
    }
}

#[derive(Debug)]
pub(crate) enum SettingsError {
    Exceeded,
    Malformed,
    Repeated(SettingId),
    InvalidSettingId(u64),
    InvalidSettingValue(SettingId, u64),
}

impl From<SettingsError> for Error {
    fn from(e: SettingsError) -> Self {
        match e {
            SettingsError::Exceeded => Error::SettingsExceeded,
            SettingsError::Malformed => Error::Malformed,
            SettingsError::Repeated(i) => Error::SettingRepeated(i),
            SettingsError::InvalidSettingId(i) => Error::InvalidSettingId(i),
            SettingsError::InvalidSettingValue(i, v) => Error::InvalidSettingValue(i, v),
        }
    }
}

impl From<UnexpectedEnd> for Error {
    fn from(_: UnexpectedEnd) -> Self {
        Error::UnexpectedEnd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn unknown_frame_type() {
        let mut buf = Cursor::new(&[22, 4, 0, 255, 128, 0, 3, 1, 2]);
        assert_eq!(Frame::decode(&mut buf), Err(Error::UnknownFrame(22)));
        assert_eq!(Frame::decode(&mut buf), Ok(Frame::CancelPush(2)));
    }

    #[test]
    fn len_unexpected_end() {
        let mut buf = Cursor::new(&[0, 255]);
        let decoded = Frame::decode(&mut buf);
        assert_eq!(decoded, Err(Error::Incomplete(3)));
    }

    #[test]
    fn type_unexpected_end() {
        let mut buf = Cursor::new(&[255]);
        let decoded = Frame::decode(&mut buf);
        assert_eq!(decoded, Err(Error::Incomplete(2)));
    }

    #[test]
    fn buffer_too_short() {
        let mut buf = Cursor::new(&[4, 4, 0, 255, 128]);
        let decoded = Frame::decode(&mut buf);
        assert_eq!(decoded, Err(Error::Incomplete(6)));
    }

    fn codec_frame_check(frame: Frame, wire: &[u8]) {
        let mut buf = Vec::new();
        frame.encode(&mut buf);
        println!("buf: {:?}", buf);
        assert_eq!(&buf, &wire);

        let mut read = Cursor::new(&buf);
        let decoded = Frame::decode(&mut read).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn settings_frame() {
        codec_frame_check(
            Frame::Settings(SettingsFrame {
                entries: [
                    (SettingId::MAX_HEADER_LIST_SIZE, 0xfad1),
                    (SettingId::QPACK_MAX_TABLE_CAPACITY, 0xfad2),
                    (SettingId::QPACK_MAX_BLOCKED_STREAMS, 0xfad3),
                ],
                len: 3,
            }),
            &[
                4, 15, 6, 128, 0, 250, 209, 1, 128, 0, 250, 210, 7, 128, 0, 250, 211,
            ],
        );
    }

    #[test]
    fn settings_frame_emtpy() {
        codec_frame_check(Frame::Settings(SettingsFrame::default()), &[4, 0]);
    }

    #[test]
    fn data_frame() {
        codec_frame_check(
            Frame::Data(DataFrame {
                payload: Bytes::from("foo bar"),
            }),
            &[0, 7, 102, 111, 111, 32, 98, 97, 114],
        );
    }

    #[test]
    fn simple_frames() {
        codec_frame_check(Frame::CancelPush(2), &[3, 1, 2]);
        codec_frame_check(Frame::Goaway(2), &[7, 1, 2]);
        codec_frame_check(Frame::MaxPushId(2), &[13, 1, 2]);
        codec_frame_check(Frame::DuplicatePush(2), &[14, 1, 2]);
    }

    #[test]
    fn headers_frames() {
        codec_frame_check(
            Frame::Headers(HeadersFrame {
                encoded: Bytes::from("TODO QPACK"),
            }),
            &[1, 10, 84, 79, 68, 79, 32, 81, 80, 65, 67, 75],
        );
        codec_frame_check(
            Frame::PushPromise(PushPromiseFrame {
                id: 134,
                encoded: Bytes::from("TODO QPACK"),
            }),
            &[5, 12, 64, 134, 84, 79, 68, 79, 32, 81, 80, 65, 67, 75],
        );
    }

    #[test]
    fn reserved_frame() {
        let mut raw = vec![];
        VarInt::from_u32(0x21 + 2 * 0x1f).encode(&mut raw);
        raw.extend(&[6, 0, 255, 128, 0, 250, 218]);
        let mut buf = Cursor::new(&raw);
        let decoded = Frame::decode(&mut buf);
        assert_eq!(decoded, Ok(Frame::Reserved));
    }
}
