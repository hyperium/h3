#![no_main]
use libfuzzer_sys::fuzz_target;
use h3::proto::varint::VarInt;
use bytes::Bytes;

fuzz_target!(|data: &[u8]| {
    let mut input = Bytes::from(data.to_vec());
    let _ = VarInt::decode(&mut input);
});
