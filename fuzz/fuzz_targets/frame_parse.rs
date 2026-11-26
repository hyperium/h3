#![no_main]

use bytes::Bytes;
use h3::proto::frame::{Frame, PayloadLen};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut buf = Bytes::from(data.to_vec());
    let _ = Frame::<PayloadLen>::decode(&mut buf);
});