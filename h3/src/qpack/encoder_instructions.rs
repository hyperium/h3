//! Types that can be sent and received in the encoder stream

use bytes::{Buf, BufMut};

use crate::error::internal_error::InternalConnectionError;
use crate::error::Code;

use super::prefix_int;
use super::prefix_int::Error as IntError;
use super::prefix_string;
use super::prefix_string::PrefixStringError as StringError;

/// Error type for parsing encoder instructions
#[derive(Debug)]
pub enum ParseEncoderInstructionError {
    /// The buffer is not long enough to read the instruction
    /// The Caller decides how to handle this because not all data may arrived yet
    UnexpectedEnd,
    /// Failed to interpret the instruction
    ConnectionError(InternalConnectionError),
}

impl ParseEncoderInstructionError {
    fn connection_error(message: String) -> Self {
        ParseEncoderInstructionError::ConnectionError(InternalConnectionError::new(
            Code::QPACK_ENCODER_STREAM_ERROR,
            message,
        ))
    }
}

// 4.3. Encoder Instructions
pub enum EncoderInstruction {
    // 4.3.1. Set Dynamic Table Capacity
    // An encoder informs the decoder of a change to the dynamic table capacity.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 1 |   Capacity (5+)   |
    // +---+---+---+-------------------+
    DynamicTableSizeUpdate(DynamicTableSizeUpdate),
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
    InsertWithNameRef(InsertWithNameRef),
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
    InsertWithoutNameRef(InsertWithoutNameRef),
    // 4.3.4. Duplicate
    // An encoder duplicates an existing entry in the dynamic table.
    //   0   1   2   3   4   5   6   7
    // +---+---+---+---+---+---+---+---+
    // | 0 | 0 | 0 |    Index (5+)     |
    // +---+---+---+-------------------+
    Duplicate(Duplicate),
    Unknown,
}

impl EncoderInstruction {
    /*pub fn decode(first: u8) -> Self {
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
    }*/

    /* fn decode<R: Buf>(read: &mut R) -> Result<Option<EncoderInstruction>, InternalConnectionError> {
        if read.remaining() < 1 {
            return Ok(None);
        }

        let mut buf = Cursor::new(read.chunk());
        let first = buf.chunk()[0];

        let x = if first & 0b1000_0000 != 0 {
            //EncoderInstruction::InsertWithNameRef
            todo!()
        } else if first & 0b0100_0000 == 0b0100_0000 {
            //EncoderInstruction::InsertWithoutNameRef
            todo!()
        } else if first & 0b1110_0000 == 0 {
            //EncoderInstruction::Duplicate
            todo!()
        } else if first & 0b0010_0000 == 0b0010_0000 {
            //EncoderInstruction::DynamicTableSizeUpdate
            let table_size_update= DynamicTableSizeUpdate::decode(&mut buf);
            EncoderInstruction::DynamicTableSizeUpdate(DynamicTableSizeUpdate::decode(&mut buf))
        } else {
            //EncoderInstruction::Unknown
            // TODO: can this be unreachable!()?
            return Err(InternalConnectionError::new(
                Code::H3_INTERNAL_ERROR,
                "Unknown EncoderInstruction".to_string(),
            ));
        };

        let instruction = match EncoderInstruction::decode(first) {
            EncoderInstruction::Unknown => return Err(DecoderError::UnknownPrefix(first)),
            EncoderInstruction::DynamicTableSizeUpdate => {
                DynamicTableSizeUpdate::decode(&mut buf)?.map(|x| Instruction::TableSizeUpdate(x.0))
            }
            EncoderInstruction::InsertWithoutNameRef => InsertWithoutNameRef::decode(&mut buf)?
                .map(|x| Instruction::Insert(HeaderField::new(x.name, x.value))),
            EncoderInstruction::Duplicate => match Duplicate::decode(&mut buf)? {
                Some(Duplicate(index)) => {
                    Some(Instruction::Insert(self.table.get_relative(index)?.clone()))
                }
                None => None,
            },
            EncoderInstruction::InsertWithNameRef => match InsertWithNameRef::decode(&mut buf)? {
                Some(InsertWithNameRef::Static { index, value }) => Some(Instruction::Insert(
                    StaticTable::get(index)?.with_value(value),
                )),
                Some(InsertWithNameRef::Dynamic { index, value }) => Some(Instruction::Insert(
                    self.table.get_relative(index)?.with_value(value),
                )),
                None => None,
            },
        };

        if instruction.is_some() {
            let pos = buf.position();
            read.advance(pos as usize);
        }

        Ok(instruction)
    }*/
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

    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseEncoderInstructionError> {
        let (flags, index) = match prefix_int::decode(6, buf) {
            Ok((f, x)) if f & 0b10 == 0b10 => (f, x),
            Ok((f, _)) => return Err(ParseError::InvalidPrefix(f)),
            Err(IntError::UnexpectedEnd) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let index: usize = index.try_into().map_err(|_e| {
            ParseEncoderInstructionError::Integer(crate::qpack::prefix_int::Error::Overflow)
        })?;

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

    pub fn encode<W: BufMut>(&self, buf: &mut W) -> Result<(), prefix_string::PrefixStringError> {
        match self {
            InsertWithNameRef::Static { index, value } => {
                prefix_int::encode(6, 0b11, *index as u64, buf);
                prefix_string::encode(8, 0, value, buf)?;
            }
            InsertWithNameRef::Dynamic { index, value } => {
                prefix_int::encode(6, 0b10, *index as u64, buf);
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

    pub fn decode<R: Buf>(buf: &mut R) -> Result<Option<Self>, ParseEncoderInstructionError> {
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

    pub fn encode<W: BufMut>(&self, buf: &mut W) -> Result<(), prefix_string::PrefixStringError> {
        prefix_string::encode(6, 0b01, &self.name, buf)?;
        prefix_string::encode(8, 0, &self.value, buf)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Duplicate(u64);

impl Duplicate {
    /// Call this function only if you are sure that the prefix is 0b000
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Self, ParseEncoderInstructionError> {
        let index = match prefix_int::decode(5, buf) {
            Ok((0, x)) => x,
            Ok(_) => {
                unreachable!("This function must not be called when the prefix is other than 0b000")
            }
            Err(IntError::UnexpectedEnd) => {
                return Err(ParseEncoderInstructionError::UnexpectedEnd)
            }
            Err(IntError::Overflow) => {
                return Err(ParseEncoderInstructionError::connection_error(
                    "Integer value to large".to_string(),
                ))
            }
        };
        Ok(Duplicate(index))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(5, 0, self.0 as u64, buf);
    }
}

#[derive(Debug, PartialEq)]
pub struct DynamicTableSizeUpdate(u64);

impl DynamicTableSizeUpdate {
    /// Call this function only if you are sure that the prefix is 0b001
    pub fn decode<R: Buf>(buf: &mut R) -> Result<Self, ParseEncoderInstructionError> {
        let size = match prefix_int::decode(5, buf) {
            Ok((0b001, x)) => x,
            Ok(_) => {
                unreachable!("This function must not be called when the prefix is other than 0b001")
            }
            Err(IntError::UnexpectedEnd) => {
                return Err(ParseEncoderInstructionError::UnexpectedEnd)
            }
            Err(IntError::Overflow) => {
                return Err(ParseEncoderInstructionError::connection_error(
                    "Integer value to large".to_string(),
                ))
            }
        };
        Ok(DynamicTableSizeUpdate(size))
    }

    pub fn encode<W: BufMut>(&self, buf: &mut W) {
        prefix_int::encode(5, 0b001, self.0 as u64, buf);
    }
}
