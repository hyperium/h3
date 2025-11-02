use bytes::{Buf, BufMut};

use crate::qpack2::{
    prefix_int::{new_prefix_int_parser, PrefixIntParseError, PrefixIntParser},
    prefix_string::huffman_decode::{HuffmanDecodingError, StatefulHuffmanDecoder},
    qpack_result::{ParseProgressResult, StatefulParser},
};

#[derive(Debug, PartialEq, thiserror::Error)]
enum StringDecodeError {
    #[error("prefix int parse error: {0}")]
    PrefixIntParseError(#[from] PrefixIntParseError),
    #[error("huffman decode error: {0}")]
    HuffmanDecodeError(#[from] HuffmanDecodingError),
}

#[derive(Debug)]
enum StatefulPrefixStringDecoder<const S: u8> {
    Integer(PrefixIntParser<S>),
    HuffmanString {
        len: u64,
        decoder: StatefulHuffmanDecoder,
        flags: u8,
    },
    PlainString {
        len: u64,
        value: Vec<u8>,
        flags: u8,
    },
}

impl<const S: u8> StatefulPrefixStringDecoder<S> {
    const INT_SIZE: u8 = S + 1;
}

fn new_prefix_string_parser<const S: u8>() -> StatefulPrefixStringDecoder<S> {
    StatefulPrefixStringDecoder::Integer(new_prefix_int_parser::<S>())
}

impl<const S: u8> StatefulParser<StringDecodeError, (u8, Vec<u8>)>
    for StatefulPrefixStringDecoder<S>
{
    fn parse_progress<B: Buf>(
        mut self,
        buf: &mut B,
    ) -> ParseProgressResult<Self, StringDecodeError, (u8, Vec<u8>)> {
        loop {
            match self {
                StatefulPrefixStringDecoder::Integer(prefix_int_parser) => {
                    match prefix_int_parser.parse_progress(buf) {
                        ParseProgressResult::MoreData(parser) => {
                            return ParseProgressResult::MoreData(Self::Integer(parser))
                        }
                        ParseProgressResult::Done((flags, len)) => {
                            if flags & 1 == 1 {
                                // Huffman encoded
                                // TODO: can we safely cast len to usize?
                                let decoder = StatefulHuffmanDecoder::new(len as usize);
                                self = StatefulPrefixStringDecoder::HuffmanString {
                                    len,
                                    decoder,
                                    // Ignore the huffman bit
                                    flags: flags >> 1,
                                };
                            } else {
                                // Plain string
                                self = StatefulPrefixStringDecoder::PlainString {
                                    len,
                                    value: Vec::with_capacity(len as usize),
                                    // Ignore the huffman bit
                                    flags: flags >> 1,
                                };
                            }
                        }
                        ParseProgressResult::Error(e) => {
                            return ParseProgressResult::Error(
                                StringDecodeError::PrefixIntParseError(e),
                            )
                        }
                    }
                }
                StatefulPrefixStringDecoder::HuffmanString {
                    len,
                    decoder,
                    flags,
                } => match decoder.parse_progress(buf) {
                    ParseProgressResult::MoreData(decoder) => {
                        return ParseProgressResult::MoreData(Self::HuffmanString {
                            len,
                            decoder,
                            flags,
                        })
                    }
                    ParseProgressResult::Done(value) => {
                        return ParseProgressResult::Done((flags, value));
                    }
                    ParseProgressResult::Error(e) => {
                        return ParseProgressResult::Error(StringDecodeError::HuffmanDecodeError(e))
                    }
                },
                StatefulPrefixStringDecoder::PlainString {
                    len,
                    ref mut value,
                    flags,
                } => {
                    let to_read = len as usize - value.len();
                    let available = buf.remaining();
                    let read = to_read.min(available);

                    let taken = buf.take(read);

                    value.put(taken);

                    if value.len() == len as usize {
                        return ParseProgressResult::Done((flags, std::mem::take(value)));
                    } else {
                        return ParseProgressResult::MoreData(Self::PlainString {
                            len,
                            value: std::mem::take(value),
                            flags,
                        });
                    }
                }
            }
        }
    }
}

/*
pub fn decode<B: Buf>(size: u8, buf: &mut B) -> Result<Vec<u8>, Error> {
    let (flags, len) = prefix_int::decode(size - 1, buf)?;
    let len: usize = len.try_into()?;
    if buf.remaining() < len {
        return Err(Error::UnexpectedEnd);
    }

    let payload = buf.copy_to_bytes(len);
    let value = if flags & 1 == 0 {
        payload.into_iter().collect()
    } else {
        let mut decoded = Vec::new();
        for byte in payload.into_iter().collect::<Vec<u8>>().hpack_decode() {
            decoded.push(byte?);
        }
        decoded
    };
    Ok(value)
}*/

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpack2::{
        prefix_int,
        prefix_string::huffman_encode::HpackStringEncode,
        qpack_result::ParseProgressResult,
    };
    use crate::tests::test_all_chunking_combinations;
    use std::io::Cursor;

    // Helper: build an encoded prefix string buffer
    // S = 7 (HPACK/QPACK string literal: 1-bit H flag + 7-bit length prefix)
    fn build_prefix_string(huffman: bool, value: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        if huffman {
            let encoded = value.to_vec().hpack_encode();
            // flags: H=1, upper flag bits=0
            prefix_int::encode(7, 1, encoded.len() as u64, &mut buf);
            buf.extend_from_slice(&encoded);
        } else {
            // flags: H=0, upper flag bits=0
            prefix_int::encode(7, 0, value.len() as u64, &mut buf);
            buf.extend_from_slice(value);
        }
        buf
    }

    #[test]
    fn decode_plain_small_all_chunkings() {
        let input = b"hello";
        let data = build_prefix_string(false, input);
        test_all_chunking_combinations(
            &mut Cursor::new(&data),
            || new_prefix_string_parser::<7>(),
            false,
            // flags are returned with H stripped (flags >> 1), so 0 here
            ParseProgressResult::Done((0u8, input.to_vec())),
        );
    }

    #[test]
    fn decode_huffman_small_all_chunkings() {
        let input = b"hello";
        let data = build_prefix_string(true, input);
        test_all_chunking_combinations(
            &mut Cursor::new(&data),
            || new_prefix_string_parser::<7>(),
            false,
            ParseProgressResult::Done((0u8, input.to_vec())),
        );
    }

    #[test]
    fn decode_plain_empty() {
        let input: &[u8] = b"";
        let data = build_prefix_string(false, input);
        test_all_chunking_combinations(
            &mut Cursor::new(&data),
            || new_prefix_string_parser::<7>(),
            false,
            ParseProgressResult::Done((0u8, input.to_vec())),
        );
    }

    #[test]
    fn decode_huffman_long_sampled_chunkings() {
        // Longer string to exercise multi-byte length encoding and chunked parsing
        let input = b"The quick brown fox jumps over the lazy dog 1234567890!";
        let data = build_prefix_string(true, input);
        test_all_chunking_combinations(
            &mut Cursor::new(&data),
            || new_prefix_string_parser::<7>(),
            true, // sampled to keep runtime reasonable
            ParseProgressResult::Done((0u8, input.to_vec())),
        );
    }
}
