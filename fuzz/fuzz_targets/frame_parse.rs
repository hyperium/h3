#![no_main]

use bytes::Buf;
use bytes::Bytes;
use h3::proto::frame::{Frame, PayloadLen};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut buf = Bytes::from(data.to_vec());

    // Decode a *stream* of frames: repeatedly parse a frame header + payload until the
    // buffer is exhausted or a decode error is returned. This exercises varint / frame
    // type / length parsing across frame boundaries and under arbitrary truncation,
    // including a frame type 0x00 (reserved), a varint length that overflows u64, and a
    // DATA/SETTINGS/HEADERS frame of arbitrary length.
    //
    // NOTE on scope: HTTP/3 frame *ordering* rules (DATA before HEADERS, HEADERS after a
    // trailing HEADERS, GOAWAY on a request stream => H3_FRAME_UNEXPECTED, a SETTINGS
    // frame in the wrong position, etc.) are enforced by the connection layer's stream
    // state machine in h3/src/connection.rs, not by the frame codec. They require a QUIC
    // connection to drive and are therefore NOT reachable from this isolated frame-codec
    // fuzz target. That sequencing logic must be fuzzed or covered by tests separately.
    while buf.has_remaining() {
        let before = buf.remaining();
        if Frame::<PayloadLen>::decode(&mut buf).is_err() {
            // Exhausted / truncated / malformed: stop rather than spin on the same bytes.
            break;
        }
        if buf.remaining() == before {
            // Guard against a decode that returns Ok() without consuming input, which
            // would otherwise loop forever.
            break;
        }
    }
});