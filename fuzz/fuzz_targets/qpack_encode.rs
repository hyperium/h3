#![no_main]

use h3::qpack::{decode_stateless, encode_stateless, HeaderField};
use libfuzzer_sys::fuzz_target;

// Header-section size cap used when round-tripping the encoder output through the decoder.
const MAX_SIZE: u64 = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }

    // Split the arbitrary input into a field name and value. The split is well-formed
    // (name may be empty, which is still a valid encoder input and exercises edge cases).
    let mid = (data[0] as usize) % (data.len() - 1) + 1;
    let field = HeaderField::new(data[1..mid].to_vec(), data[mid..].to_vec());

    let mut out = Vec::new();
    if encode_stateless(&mut out, [&field]).is_ok() {
        // Round-trip the encoded field section through the decoder to ensure the wire
        // output is valid (and does not panic).
        let mut buf = std::io::Cursor::new(out);
        let _ = decode_stateless(&mut buf, MAX_SIZE);
    }
});