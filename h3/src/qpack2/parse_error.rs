use super::{prefix_int, prefix_string};

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Integer(prefix_int::Error),
    String(prefix_string::PrefixStringError),
    InvalidPrefix(u8),
    InvalidBase(isize),
}

impl From<prefix_int::Error> for ParseError {
    fn from(e: prefix_int::Error) -> Self {
        ParseError::Integer(e)
    }
}

impl From<prefix_string::PrefixStringError> for ParseError {
    fn from(e: prefix_string::PrefixStringError) -> Self {
        ParseError::String(e)
    }
}
