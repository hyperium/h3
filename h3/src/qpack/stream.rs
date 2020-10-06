use bytes::{Buf, BufMut};

use super::{
    parse_error::ParseError,
    prefix_int::{self, Error as IntError},
    prefix_string::{self, Error as StringError},
};

// 4.3. Encoder Instructions
pub enum EncoderInstruction {
    // 4.3.1. Set Dynamic Table Capacity
    // An encoder informs the decoder of a change to the dynamic table capacity.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 1 |   Capacity (5+)   |
    // +---+---+---+-------------------+
    DynamicTableSizeUpdate,
    // 4.3.2. Insert With Name Reference
    // An encoder adds an entry to the dynamic table where the field name
    // matches the field name of an entry stored in the static or the dynamic
    // table.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 1 | T |    Name Index (6+)    |
    // +---+---+-----------------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    InsertWithNameRef,
    // 4.3.3. Insert With Literal Name
    // An encoder adds an entry to the dynamic table where both the field name
    // and the field value are represented as string literals.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 1 | H | Name Length (5+)  |
    // +---+---+---+-------------------+
    // |  Name String (Length bytes)   |
    // +---+---------------------------+
    // | H |     Value Length (7+)     |
    // +---+---------------------------+
    // |  Value String (Length bytes)  |
    // +-------------------------------+
    InsertWithoutNameRef,
    // 4.3.4. Duplicate
    // An encoder duplicates an existing entry in the dynamic table.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 |    Index (5+)     |
    // +---+---+---+-------------------+
    Duplicate,
    Unknown,
}

impl EncoderInstruction {
    pub fn decode(first: u8) -> Self {
        if first & 0b1000_0000 != 0 {
            EncoderInstruction::InsertWithNameRef
        } else if first & 0b0100_0000 == 0b0100_0000 {
            EncoderInstruction::InsertWithoutNameRef
        } else if first & 0b1110_0000 == 0 {
            EncoderInstruction::Duplicate
        } else if first & 0b0010_0000 == 0b0010_0000 {
            EncoderInstruction::DynamicTableSizeUpdate
        } else {
            EncoderInstruction::Unknown
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum InsertWithNameRef {
    Static { index: usize, value: Vec<u8> },
    Dynamic { index: usize, value: Vec<u8> },
}

impl InsertWithNameRef {
    pub fn new_static<T: Into<Vec<u8>>>(index: usize, value: T) -> Self {
        InsertWithNameRef::Static {
            index,
            value: value.into(),
        }
    }

    pub fn new_dynamic<T: Into<Vec<u8>>>(index: usize, value: T) -> Self {
        InsertWithNameRef::Dynamic {
            index,
            value: value.into(),
        }
    }

    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let (flags, index) = match prefix_int::decode(6, buf) {
            Ok((f, x)) if f & 0b10 == 0b10 => (f, x),
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let value = match prefix_string::decode(8, buf) {
            Ok(x) => x,
            Err(StringError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        if flags & 0b01 == 0b01 {
            Ok(Some(InsertWithNameRef::new_static(index, value)))
        } else {
            Ok(Some(InsertWithNameRef::new_dynamic(index, value)))
        }
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) -> Result<(), prefix_string::Error> {
        match self {
            InsertWithNameRef::Static { index, value } => {
                prefix_int::encode(6, 0b11, *index, buf);
                prefix_string::encode(8, 0, value, buf)?;
            }
            InsertWithNameRef::Dynamic { index, value } => {
                prefix_int::encode(6, 0b10, *index, buf);
                prefix_string::encode(8, 0, value, buf)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct InsertWithoutNameRef {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

impl InsertWithoutNameRef {
    pub fn new<T: Into<Vec<u8>>>(name: T, value: T) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let name = match prefix_string::decode(6, buf) {
            Ok(x) => x,
            Err(StringError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let value = match prefix_string::decode(8, buf) {
            Ok(x) => x,
            Err(StringError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(Self::new(name, value)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) -> Result<(), prefix_string::Error> {
        prefix_string::encode(6, 0b01, &self.name, buf)?;
        prefix_string::encode(8, 0, &self.value, buf)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Duplicate(pub usize);

impl Duplicate {
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let index = match prefix_int::decode(5, buf) {
            Ok((0, x)) => x,
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(Duplicate(index)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(5, 0, self.0, buf);
    }
}

#[derive(Debug, PartialEq)]
pub struct DynamicTableSizeUpdate(pub usize);

impl DynamicTableSizeUpdate {
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let size = match prefix_int::decode(5, buf) {
            Ok((0b001, x)) => x,
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(DynamicTableSizeUpdate(size)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(5, 0b001, self.0, buf);
    }
}

// 4.4. Decoder Instructions
// A decoder sends decoder instructions on the decoder stream to inform the encoder
// about the processing of field sections and table updates to ensure consistency
// of the dynamic table.
#[derive(Debug, PartialEq)]
pub enum DecoderInstruction {
    // 4.4.1. Section Acknowledgement
    // Acknowledge processing of an encoded field section whose declared Required
    // Insert Count is not zero.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 1 |      Stream ID (7+)       |
    // +---+---------------------------+
    HeaderAck,
    // 4.4.2. Stream Cancellation
    // When a stream is reset or reading is abandoned.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 1 |     Stream ID (6+)    |
    // +---+---+-----------------------+
    StreamCancel,
    //  4.4.3. Insert Count Increment
    //  Increases the Known Received Count to the total number of dynamic table
    //  insertions and duplications processed so far.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 |     Increment (6+)    |
    // +---+---+-----------------------+
    InsertCountIncrement,
    Unknown,
}

impl DecoderInstruction {
    pub fn decode(first: u8) -> Self {
        if first & 0b1100_0000 == 0 {
            DecoderInstruction::InsertCountIncrement
        } else if first & 0b1000_0000 != 0 {
            DecoderInstruction::HeaderAck
        } else if first & 0b0100_0000 == 0b0100_0000 {
            DecoderInstruction::StreamCancel
        } else {
            DecoderInstruction::Unknown
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct InsertCountIncrement(pub usize);

impl InsertCountIncrement {
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let insert_count = match prefix_int::decode(6, buf) {
            Ok((0b00, x)) => x,
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(InsertCountIncrement(insert_count)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(6, 0b00, self.0, buf);
    }
}

#[derive(Debug, PartialEq)]
pub struct HeaderAck(pub u64);

impl HeaderAck {
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let stream_id = match prefix_int::decode(7, buf) {
            Ok((0b1, x)) => x as u64,
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(HeaderAck(stream_id)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(7, 0b1, self.0 as usize, buf);
    }
}

#[derive(Debug, PartialEq)]
pub struct StreamCancel(pub u64);

impl StreamCancel {
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseError> {
        let stream_id = match prefix_int::decode(6, buf) {
            Ok((0b01, x)) => x as u64,
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some(StreamCancel(stream_id)))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(6, 0b01, self.0 as usize, buf);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn insert_with_name_ref() {
        let instruction = InsertWithNameRef::new_static(0, "value");
        let mut buf = vec![];
        instruction.encode(&mut buf).unwrap();
        let mut read = Cursor::new(&buf);
        assert_eq!(InsertWithNameRef::decode(&mut read), Ok(Some(instruction)));
    }

    #[test]
    fn insert_without_name_ref() {
        let instruction = InsertWithoutNameRef::new("name", "value");
        let mut buf = vec![];
        instruction.encode(&mut buf).unwrap();
        let mut read = Cursor::new(&buf);
        assert_eq!(
            InsertWithoutNameRef::decode(&mut read),
            Ok(Some(instruction))
        );
    }

    #[test]
    fn insert_duplicate() {
        let instruction = Duplicate(42);
        let mut buf = vec![];
        instruction.encode(&mut buf);
        let mut read = Cursor::new(&buf);
        assert_eq!(Duplicate::decode(&mut read), Ok(Some(instruction)));
    }

    #[test]
    fn dynamic_table_size_update() {
        let instruction = DynamicTableSizeUpdate(42);
        let mut buf = vec![];
        instruction.encode(&mut buf);
        let mut read = Cursor::new(&buf);
        assert_eq!(
            DynamicTableSizeUpdate::decode(&mut read),
            Ok(Some(instruction))
        );
    }

    #[test]
    fn insert_count_increment() {
        let instruction = InsertCountIncrement(42);
        let mut buf = vec![];
        instruction.encode(&mut buf);
        let mut read = Cursor::new(&buf);
        assert_eq!(
            InsertCountIncrement::decode(&mut read),
            Ok(Some(instruction))
        );
    }

    #[test]
    fn header_ack() {
        let instruction = HeaderAck(42);
        let mut buf = vec![];
        instruction.encode(&mut buf);
        let mut read = Cursor::new(&buf);
        assert_eq!(HeaderAck::decode(&mut read), Ok(Some(instruction)));
    }

    #[test]
    fn stream_cancel() {
        let instruction = StreamCancel(42);
        let mut buf = vec![];
        instruction.encode(&mut buf);
        let mut read = Cursor::new(&buf);
        assert_eq!(StreamCancel::decode(&mut read), Ok(Some(instruction)));
    }
}
