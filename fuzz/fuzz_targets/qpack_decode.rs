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
        // data[0] is reserved purely as the split control byte, so the fuzzer can
        // independently vary "how long the setup is" vs "what opcode it starts with".
        // split ∈ [0, len-1]; data[1..] has len-1 bytes, so setup may be empty and the
        // first encoder-instruction byte is data[1], decoupled from the split value.
        let split = (data[0] as usize) % data.len();
        let (setup, section) = data[1..].split_at(split);

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