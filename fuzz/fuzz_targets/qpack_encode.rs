#![no_main]

use h3::qpack::{decode_stateless, encode_stateless, HeaderField};
use libfuzzer_sys::fuzz_target;

// Header-section size cap used when round-tripping the encoder output through the decoder.
const MAX_SIZE: u64 = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }

    // Split the arbitrary input into a field name and value. data[0] is reserved purely
    // as the split point so the name/value bytes are decoupled from the control byte.
    // mid ∈ [1, len], so the value may be empty (mid == len) and the name may be empty
    // (mid == 1) — both are valid encoder inputs.
    let mid = (data[0] as usize) % data.len() + 1;
    let raw_name = &data[1..mid];
    let raw_value = &data[mid..];

    // Field names are lowercased by the QPACK/HTTP layer (RFC 9114 §4.2 rejects
    // uppercase in field names). Encode a lowercase name so encode_stateless accepts it
    // and the round-trip assertion below compares like-for-like instead of failing on a
    // name that is legal to the encoder but illegal on the wire.
    let mut name = raw_name.to_vec();
    name.make_ascii_lowercase();
    let field = HeaderField::new(name, raw_value.to_vec());

    let mut out = Vec::new();
    if encode_stateless(&mut out, [&field]).is_ok() {
        // Round-trip the encoded field section through the decoder and ASSERT the output
        // matches the input. This is what actually catches an encoder bug such as the
        // historical name-prefix bit-width error (encoder emits a name/value of the wrong
        // length) — a mismatch that would silently pass if the decode result were ignored.
        let mut buf = std::io::Cursor::new(out);
        match decode_stateless(&mut buf, MAX_SIZE) {
            Ok(decoded) => {
                assert_eq!(decoded.fields, vec![field], "encoder output did not round-trip")
            }
            Err(e) => panic!("encode_stateless produced undecodable bytes: {e:?}"),
        }
    }
});