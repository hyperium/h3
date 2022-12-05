use std::fmt;

use bytes::{Buf, BufMut};

use crate::proto::{
    frame::{FrameError, FrameHeader, FrameType},
    stream::InvalidStreamId,
    varint::{BufExt, BufMutExt, UnexpectedEnd, VarInt},
};

#[derive(Debug, PartialEq)]
pub struct Settings {
    len: usize,
    buf: [(u64, u64); Self::MAX_LEN],
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            len: 0,
            buf: [(0, 0); Self::MAX_LEN],
        }
    }
}

impl FrameHeader for Settings {
    const TYPE: FrameType = FrameType::SETTINGS;

    fn len(&self) -> usize {
        self.buf[..self.len].iter().fold(0, |len, (id, val)| {
            len + VarInt::from_u64(*id).unwrap().size() + VarInt::from_u64(*val).unwrap().size()
        })
    }
}

impl Settings {
    pub const QPACK_MAX_TABLE_CAPACITY: u64 = 0x1;
    pub const MAX_HEADER_LIST_SIZE: u64 = 0x6;
    pub const QPACK_MAX_BLOCKED_STREAMS: u64 = 0x7;
    pub const ENABLE_WEBTRANSPORT: u64 = 0x2b603742;

    // 4 setting types plus GREASE
    pub const MAX_LEN: usize = 5;
    pub const MAX_ENCODED_SIZE: usize = 2 * Self::MAX_LEN * VarInt::MAX_SIZE;

    pub fn insert(&mut self, id: u64, val: u64) -> Result<(), SettingsError> {
        if !Self::is_supported(id) {
            Err(SettingsError::InvalidSettingId(id))?;
        }

        // TODO check val?

        self.do_insert(id, val)
    }

    pub fn insert_grease(&mut self, val: u64) -> Result<(), SettingsError> {
        let id = 0x1f * fastrand::u64(0..0x210842108421083) + 0x21;
        self.do_insert(id, val)
    }

    pub fn get(&self, maybe_id: u64) -> Option<u64> {
        for (id, val) in self.buf[..self.len].iter() {
            if maybe_id == *id {
                return Some(*val);
            }
        }
        None
    }

    pub(super) fn encode<B: BufMut>(&self, buf: &mut B) {
        self.encode_header(buf);
        for (id, val) in self.buf[..self.len].iter() {
            buf.write_var(*id);
            buf.write_var(*val);
        }
    }

    pub(super) fn decode<B: Buf>(buf: &mut B) -> Result<Self, SettingsError> {
        let mut settings = Settings::default();

        while buf.has_remaining() {
            if buf.remaining() < 2 {
                // remains less than 2 * minimum-size varint
                Err(SettingsError::Malformed)?;
            }

            let id = buf.get_var().map_err(|_| SettingsError::Malformed)?;
            let val = buf.get_var().map_err(|_| SettingsError::Malformed)?;

            if Self::is_reserved(id) {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4.1
                //= type=TODO
                //# Setting identifiers that were defined in [HTTP/2] where there is no
                //# corresponding HTTP/3 setting have also been reserved
                //# (Section 11.2.2).  These reserved settings MUST NOT be sent, and
                //# their receipt MUST be treated as a connection error of type
                //# H3_SETTINGS_ERROR.
                Err(SettingsError::InvalidSettingId(id))?;
            }

            if Self::is_supported(id) {
                settings.insert(id, val)?;
            }
        }

        Ok(settings)
    }

    fn is_supported(id: u64) -> bool {
        matches!(
            id,
            Self::QPACK_MAX_TABLE_CAPACITY
                | Self::MAX_HEADER_LIST_SIZE
                | Self::QPACK_MAX_BLOCKED_STREAMS
                | Self::ENABLE_WEBTRANSPORT
        )
    }

    fn is_reserved(id: u64) -> bool {
        // https://www.rfc-editor.org/rfc/rfc9114#iana-setting-table
        matches!(id, 0 | 2 | 3 | 4 | 5)
    }

    #[inline]
    fn do_insert(&mut self, id: u64, val: u64) -> Result<(), SettingsError> {
        if self.len >= self.buf.len() {
            Err(SettingsError::Exceeded)?;
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-7.2.4
        //# The same setting identifier MUST NOT occur more than once in the
        //# SETTINGS frame.
        if self.buf[..self.len].iter().any(|(i, _)| *i == id) {
            Err(SettingsError::Repeated(id))?;
        }

        self.buf[self.len] = (id, val);
        self.len += 1;

        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub enum SettingsError {
    Exceeded,
    Malformed,
    Repeated(u64),
    InvalidSettingId(u64),
    InvalidSettingValue(u64, u64),
}

impl std::error::Error for SettingsError {}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::Exceeded => write!(
                f,
                "max settings number exceeded, check for duplicate entries"
            ),
            SettingsError::Malformed => write!(f, "malformed settings frame"),
            SettingsError::Repeated(id) => write!(f, "got setting 0x{:x} twice", id),
            SettingsError::InvalidSettingId(id) => write!(f, "setting id 0x{:x} is invalid", id),
            SettingsError::InvalidSettingValue(id, val) => {
                write!(f, "setting 0x{:x} has invalid value {}", id, val)
            }
        }
    }
}

impl From<SettingsError> for FrameError {
    fn from(e: SettingsError) -> Self {
        Self::Settings(e)
    }
}

impl From<UnexpectedEnd> for FrameError {
    fn from(e: UnexpectedEnd) -> Self {
        FrameError::Incomplete(e.0)
    }
}

impl From<InvalidStreamId> for FrameError {
    fn from(e: InvalidStreamId) -> Self {
        FrameError::InvalidStreamId(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::frame::Frame;
    use bytes::Bytes;
    use std::io::Cursor;

    fn codec_frame_check(mut frame: Frame<Bytes>, wire: &[u8], check_frame: Frame<Bytes>) {
        let mut buf = Vec::new();
        frame.encode_with_payload(&mut buf);
        assert_eq!(&buf, &wire);

        let mut read = Cursor::new(&buf);
        let decoded = Frame::decode(&mut read).unwrap();
        assert_eq!(check_frame, decoded);
    }

    #[test]
    fn settings_frame() {
        codec_frame_check(
            Frame::Settings(Settings {
                len: 4,
                buf: [
                    (Settings::MAX_HEADER_LIST_SIZE, 0xfad1),
                    (Settings::QPACK_MAX_TABLE_CAPACITY, 0xfad2),
                    (Settings::QPACK_MAX_BLOCKED_STREAMS, 0xfad3),
                    (95, 0),
                    (0, 0),
                ],
            }),
            &[
                4, 18, 6, 128, 0, 250, 209, 1, 128, 0, 250, 210, 7, 128, 0, 250, 211, 64, 95, 0,
            ],
            Frame::Settings(Settings {
                len: 3,
                buf: [
                    (Settings::MAX_HEADER_LIST_SIZE, 0xfad1),
                    (Settings::QPACK_MAX_TABLE_CAPACITY, 0xfad2),
                    (Settings::QPACK_MAX_BLOCKED_STREAMS, 0xfad3),
                    // check without the Grease setting because this is ignored
                    (0, 0),
                    (0, 0),
                ],
            }),
        );
    }

    #[test]
    fn settings_frame_empty() {
        codec_frame_check(
            Frame::Settings(Settings::default()),
            &[4, 0],
            Frame::Settings(Settings::default()),
        );
    }
}
