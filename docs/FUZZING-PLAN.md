# Fuzzing Plan: Extending the existing `h3` cargo-fuzz setup

Worktree: `/home/franck-ws/projects/h3-wt-fuzz` (branch `feat/fuzz-qpack-frame`)
Goal: add QPACK field-section and HTTP/3 frame parsing fuzz targets to the repo's existing
`fuzz/` setup without recreating it.

## 1. Existing setup

The repo already ships a standalone cargo-fuzz crate under `fuzz/` (it declares its own
`[workspace] members = ["."]` so it is excluded from the main workspace). Files:

- `fuzz/Cargo.toml` — package `h3-fuzz`, deps `libfuzzer-sys = "0.4"` and `bytes = "1.5"`,
  plus one `[[bin]]`:
  ```toml
  [[bin]]
  name = "fuzz_varint"
  path = "fuzz_targets/fuzz_varint.rs"
  test = false
  doc = false
  ```
- `fuzz/fuzz_targets/fuzz_varint.rs` — `#![no_main]`, pulls the two small, self-contained
  modules in via `#[path = "../../h3/src/..."]`:
  ```rust
  #[path = "../../h3/src/proto/varint.rs"] mod varint;
  #[path = "../../h3/src/proto/coding.rs"] mod coding;
  fuzz_target!(|data: &[u8]| { let mut input = Bytes::from(data.to_vec()); let _ = VarInt::decode(&mut input); });
  ```
- `fuzz/corpus/fuzz_varint/` — tracked seed inputs.
- `fuzz/.gitignore` — ignores `target` and `artifacts`.

The `#[path]` include pattern is only practical for tiny leaf modules. The QPACK decoder and
the HTTP/3 frame parser depend on the whole `crate::proto::*` tree, the `http` crate and
`bytes`; inlining them via `#[path]` into a fuzz crate would break their internal
`crate::...` paths. So the new targets use a **path dependency on the `h3` crate** instead.

## 2. Public API to fuzz

The needed symbols are already `pub` but only reachable when the crate is built with the
existing feature
`i-implement-a-third-party-backend-and-opt-into-breaking-changes` (this feature just turns the
otherwise-private `proto`/`qpack`/`frame` modules public; no behavior change). We enable it via
the path dependency.

### QPACK field-section decoder
`h3/src/qpack/decoder.rs`:

- `pub fn decode_stateless<T: Buf>(buf: &mut T, max_size: u64) -> Result<Decoded, DecoderError>`
  — stateless (no dynamic table). Rejects any dynamic-table reference with `MissingRefs(0)`,
  exercises prefixed-integer parsing, Huffman decoding, static-index bounds and the
  header-too-long limit.
- `pub fn decode_header<T: Buf>(&self, buf: &mut T) -> Result<Decoded, DecoderError>` on
  `pub struct Decoder` — stateful path that resolves **dynamic-table indexes** (relative and
  post-base), the required insert count / blocked-decode check (`MissingRefs`), prefixed ints
  and Huffman. This is the path that covers dynamic-table out-of-bounds index resolution.

`Decoder` is currently declared `pub` but the `decoder` module is private, so `Decoder` is
**not reachable** outside the crate, and there is no public constructor (its
`From<DynamicTable>` impl is `#[cfg(test)]`). To fuzz `decode_header` we make two minimal
additions to the crate (documented, no behavior change):

1. Re-export `Decoder` from `h3/src/qpack/mod.rs` (alongside the already-exported
   `decode_stateless`, `Decoded`, `DecoderError`).
2. Add `impl Default for Decoder` in `decoder.rs` that constructs a decoder backed by an empty
   dynamic table (`DynamicTable::new()`). With an empty table any dynamic reference in the
   input yields an out-of-bounds `DecoderError`, which is exactly the OOB coverage we want; the
   corpus can also hold encoder-instruction bytes if we later want to seed a populated table.

### HTTP/3 frame parser
`h3/src/proto/frame.rs`:

- `pub fn decode<T: Buf>(buf: &mut T) -> Result<Self, FrameError>` on
  `pub struct Frame<PayloadLen>`, exported as `h3::proto::frame::Frame`. Covers frame-type
  validation, varint length overflow (`Incomplete`/`Malformed`), reserved/unknown frame types
  (`UnknownFrame`), forbidden H2 frames (`UnsupportedFrame`), and per-frame payload parsing
  (Settings, Goaway, CancelPush, MaxPushId, PushPromise).

The sequence-level `H3_FRAME_UNEXPECTED` rules (DATA-before-HEADERS, HEADERS-after-trailing-
HEADERS, GOAWAY-on-request-stream) are **stateful** checks implemented in the connection /
request-stream code (`h3/src/server/request.rs`, `h3/src/client/stream.rs`, etc.) and are not
pure byte->result functions; they are not directly fuzzable as a stateless `&[u8]` target.
`Frame::decode` is the pure byte-level frame parser and is the right fuzz surface. This is
documented as a limitation.

### QPACK encoder (optional)
`h3/src/qpack/encoder.rs`:

- `pub fn encode_stateless<W, T, H>(block: &mut W, fields: T) -> Result<u64, EncoderError>` where
  `H: AsRef<HeaderField>` — encodes a field section from an iterator of `HeaderField`s. The
  target synthesizes `HeaderField`s from arbitrary bytes, encodes, then round-trips the output
  back through `decode_stateless` to check wire validity.

## 3. New targets

| Target file | Entry point fuzzed | Coverage |
|---|---|---|
| `fuzz/fuzz_targets/qpack_decode.rs` | `Decoder::decode_header` (first bytes fed to `on_encoder_recv` to populate/size the table; rest decoded as a field section) + `decode_stateless` | prefixed-int overflow, dynamic/static index OOB, Huffman, required insert count / blocked decode, header-too-long |
| `fuzz/fuzz_targets/frame_parse.rs` | `Frame::<PayloadLen>::decode` | frame-type validation, varint length overflow, reserved/forbidden/unknown types, per-frame payloads |
| `fuzz/fuzz_targets/qpack_encode.rs` | `encode_stateless` + round-trip `decode_stateless` | encoder wire output validity |

Each follows the `fuzz_varint` conventions: `#![no_main]` + `libfuzzer_sys::fuzz_target!`,
guaranteeing the target returns (an error or a valid value) and never panics/hangs.

## 4. Wiring in `fuzz/Cargo.toml`

Add the path dependency and three `[[bin]]` entries mirroring `fuzz_varint`:

```toml
[dependencies]
libfuzzer-sys = "0.4"
bytes = "1.5"
h3 = { path = "../h3", features = ["i-implement-a-third-party-backend-and-opt-into-breaking-changes"] }

[[bin]]
name = "qpack_decode"
path = "fuzz_targets/qpack_decode.rs"
test = false
doc = false

[[bin]]
name = "frame_parse"
path = "fuzz_targets/frame_parse.rs"
test = false
doc = false

[[bin]]
name = "qpack_encode"
path = "fuzz_targets/qpack_encode.rs"
test = false
doc = false
```

## 5. Seed corpora

Seeds are hand-crafted bytes placed under `fuzz/corpus/<target>/`:

### `fuzz/corpus/qpack_decode/`
- `static_get` — `:method: GET` static-index reference (valid static-section example).
- `dynamic_oob` — a field block whose post-base index references a dynamic entry on an
  empty table → `BadPostbaseIndex`.
- `literal_dynamic_ref` — a literal-with-name-ref to a dynamic entry → `BadRelativeIndex`
  (stateful) and `MissingRefs(0)` (stateless).
- `all_ones_overflow` — prefixed integer with all-ones continuation (integer-overflow probe).
- `huffman_invalid` — invalid/truncated Huffman sequence.
- `header_too_long` — many literal fields exceeding the size limit.
- `empty` — empty input.

### `fuzz/corpus/frame_parse/`
- `settings` — a valid SETTINGS frame.
- `empty_headers` — an empty HEADERS frame.
- `zero_data` — a zero-length DATA frame.
- `reserved_type` — reserved frame type `0x00`.
- `varint_overflow` — length field varint overflow.
- `forbidden_h2` — an H2 PRIORITY/PING frame (unsupported).
- `grease` — a grease / unknown frame type.
- `empty` — empty input.

### `fuzz/corpus/qpack_encode/`
- `simple_field` — a normal name/value pair (round-trips).
- `empty` — empty input (no fields).
- `large_value` — a large literal value (name-prefix bit-width path).

(Seed files are generated programmatically as documented in this report; the exact bytes are
listed there.)

## 6. CI integration

- GitHub Actions already runs cargo workflows in `.github/workflows/` (cargo-fuzz is not
  currently wired). Proposed: a nightly-only job that uses `rustup` to install
  `nightly-<version>` + `cargo-fuzz` and runs `cargo fuzz build` (or, if no corpus run is
  wanted, at least `cargo fuzz check`/`cargo build`) for each target so the targets stay
  compilable on every PR. Optionally a short bounded `cargo fuzz run <target> -- -runs=0`
  smoke check that imports the seed corpus and runs a fixed number of executions to catch
  panics. Note `libfuzzer-sys` requires nightly; the stable toolchain cannot build the fuzz
  crate.
- Keep the fuzz crate standalone (its own `[workspace]`) so it does not slow down the main
  workspace's stable builds.

## 7. Verification

Toolchain available on this machine: **stable only** (cargo 1.98.1, no nightly / no
`cargo-fuzz`), so `cargo fuzz run`/`build` cannot run here. Verification approach:

- `cargo check --features i-implement-a-third-party-backend-and-opt-into-breaking-changes`
  in `h3/` to confirm the `Decoder` re-export and `Default` impl compile and the public API
  surface the targets rely on exists.
- Attempt `cargo build` inside `fuzz/`; record what fails (expected: libfuzzer-sys needs
  nightly / clang sanitizer).
- Confirm the fuzz-target Rust code type-checks against the public API by keeping the usage
  minimal and mirroring the existing target's structure.

## 8. Files changed

- `h3/src/qpack/mod.rs` — re-export `Decoder`.
- `h3/src/qpack/decoder.rs` — add `impl Default for Decoder`.
- `fuzz/Cargo.toml` — add `h3` path dep + 3 `[[bin]]` entries.
- `fuzz/fuzz_targets/qpack_decode.rs` (new)
- `fuzz/fuzz_targets/frame_parse.rs` (new)
- `fuzz/fuzz_targets/qpack_encode.rs` (new)
- `fuzz/corpus/qpack_decode/*` (new seeds)
- `fuzz/corpus/frame_parse/*` (new seeds)
- `fuzz/corpus/qpack_encode/*` (new seeds)
- `docs/FUZZING-PLAN.md` (this file)
- `fuzz/.gitignore` — reviewed; existing `target`/`artifacts` entries already cover
  fuzzer-generated artifacts (corpus seeds are intentionally kept tracked).