#![no_main]

use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use h3::qpack::{decode_stateless, Decoder};

// Arbitrary header-section size cap used by the stateless decoder.
const MAX_SIZE: u64 = 4096;

fuzz_target!(|data: &[u8]| {
    // Path 1: stateful decoder. The first bytes are treated as QPACK *encoder-stream*
    // instructions (table size updates / insertions), and the remainder as an encoded
    // field section. This exercises:
    //   - dynamic-table insertion and size updates,
    //   - dynamic-table index resolution (relative & post-base) incl. out-of-bounds,
    //   - required insert count / blocked decoding (MissingRefs when the table cannot
    //     satisfy the required count),
    //   - prefixed-integer parsing (incl. overflow),
    //   - Huffman decoding (invalid/truncated sequences),
    //   - base-index handling.
    let mut decoder = Decoder::default();
    if data.is_empty() {
        let mut block = Bytes::new();
        let _ = decoder.decode_header(&mut block);
    } else {
        let n = (data[0] as usize).min(data.len() - 1);
        let (setup, section) = data.split_at(n + 1);

        let mut enc = Bytes::from(setup.to_vec());
        let mut enc_out = Vec::new();
        // Feed encoder-stream instructions; errors (e.g. unknown prefix) are fine.
        let _ = decoder.on_encoder_recv(&mut enc, &mut enc_out);

        let mut block = Bytes::from(section.to_vec());
        let _ = decoder.decode_header(&mut block);
    }

    // Path 2: stateless decoder (no dynamic table). Exercises static-index bounds, Huffman,
    // prefixed-integer parsing, and the header-too-long limit.
    let mut buf = Bytes::from(data.to_vec());
    let _ = decode_stateless(&mut buf, MAX_SIZE);
});