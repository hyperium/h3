use bytes::{Buf, BufMut};

use crate::{
    proto::coding::BufMutExt,
    qpack2::{prefix_int, prefix_string::huffman_encode::HpackStringEncode},
};

pub fn encode<B: BufMut>(size: u8, flags: u8, value: &[u8], buf: &mut B) -> () {
    let encoded = Vec::from(value).hpack_encode();
    prefix_int::encode(
        size - 1,
        flags << 1 | 1,
        // Can safely cast to u64. No one sends headers this large
        encoded.len() as u64,
        buf,
    );

    for byte in encoded {
        buf.write(byte);
    }
    ()
}
