# HTTP/3 Spec Compliance Plan — Header Validation & Critical-Stream Closure

**Branch:** `feat/header-validation`
**Scope:** `h3/src/` only (no `fuzz/`, no changelog, no version bumps)
**References:** RFC 9114 §4.2 / §4.2.1 / §4.3 / §8.1, RFC 9204, upstream `hyperium/h3` PR #364, aioquic PRs #725/#718.

This document is written *before* any code changes and walked through internally (see the
"internal review" note at the end) to lock down the design.

---

## 1. Goals

Close two spec gaps in the `h3` crate:

1. **Gap 1 — Header validation (RFC 9114 §4.2, §4.2.1, §4.3).**
   Validate QPACK-decoded field sections on *both* server request headers and client response
   headers (and trailers), surfacing a **stream error** `H3_MESSAGE_ERROR` — never a connection
   error.
2. **Gap 2 — Critical-stream-closure detection (RFC 9114 §8.1).**
   When the peer FINs or RESET_STREAMs the control stream **or** the QPACK encoder/decoder
   streams, treat it as a **connection error** `H3_CLOSED_CRITICAL_STREAM`.

---

## 2. RFC rule set

### 2.1 Header validation (RFC 9114 §4.2)
- An endpoint MUST NOT generate a field section containing connection-specific fields; any
  message containing them MUST be treated as **malformed**.
  - Connection-specific names: `Connection`, `Keep-Alive`, `Proxy-Connection`,
    `Transfer-Encoding`, `Upgrade`.
- `TE` is the only exception, and only in **requests**: when present it MUST have value exactly
  `trailers`. `TE` in **responses** (or trailers) is always invalid.
- RFC 9114 §4.2.1: multiple `Cookie` field lines MUST be concatenated into a single byte string
  with the two-byte delimiter `"; "` (0x3b 0x20) before passing to a non-HTTP/2, non-HTTP/3
  context.

### 2.2 Pseudo-header rules (RFC 9114 §4.3)
- Pseudo-header fields MUST appear before all regular fields (already enforced →
  `HeaderError::PseudoAfterRegularField`).
- Pseudo-header fields MUST NOT appear in **trailer** sections.
- Request pseudo-headers must not appear in responses and vice-versa (existing design stores them
  in a `Pseudo` struct; the response/request part-conversion already enforces presence rules).

### 2.3 Critical-stream closure (RFC 9114 §8.1)
- The control stream, QPACK encoder stream, and QPACK decoder stream are "critical" streams.
  If the peer closes (FIN) or resets (RESET_STREAM / STOP_SENDING) one of them, the endpoint MUST
  treat it as a **connection error** of type `H3_CLOSED_CRITICAL_STREAM` (0x104).

> **Error-code naming judgment call.** The task text says "H3_STREAM_CLOSED". RFC 9114 §8.1 does
> not define `H3_STREAM_CLOSED`; it defines **`H3_CLOSED_CRITICAL_STREAM` (0x104)** for exactly
> this situation. The crate already exposes `Code::H3_CLOSED_CRITICAL_STREAM` and already uses it
> for the control-stream case (see `connection.rs` `poll_control`, and test
> `control_close_send_error`). We therefore use `Code::H3_CLOSED_CRITICAL_STREAM`. This is a
> deliberate judgment call to stay RFC-correct and consistent with the existing code.

---

## 3. Where validation is inserted (function/file locations)

### Gap 1 — `h3/src/proto/headers.rs`
The single `Header` / `Field` module is the shared choke point for QPACK-decoded field sections and
for outgoing message construction. Doing validation here means both the server (request path) and
the client (response path) inherit it, and the existing call sites already map `HeaderError` to a
`H3_MESSAGE_ERROR` **stream error**.

1. Extend `HeaderError`:
   - `ConnectionSpecificHeader(String)`
   - `InvalidTeHeader`
   - `PseudoInTrailer`
   plus `Display` arms.
2. Add private helpers:
   - `fn is_connection_specific(name: &[u8]) -> bool`
   - `fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8]`
3. `Field::parse` (regular-header branch, `name[0] != b':'`):
   - Build the `HeaderName` once.
   - If `is_connection_specific(name)` → `Err(ConnectionSpecificHeader)`.
   - If name is `te` and trimmed value != `trailers` (ASCII case-insensitive) →
     `Err(InvalidTeHeader)`. (Value == `trailers` is accepted here because context — request vs
     response — is unknown; the *response* constructor / `into_response_parts` rejects `te`
     outright.)
4. `TryFrom<Vec<HeaderField>> for Header`:
   - In the `Field::Header((n, v))` arm, **concatenate duplicate `Cookie` headers** using `"; "`,
     via `fields.entry(header::COOKIE)`. Other headers keep the existing `fields.append`.
5. Constructors now return `Result<Self, HeaderError>`:
   - `request(..)` → reject connection-specific + `te` != `trailers`.
   - `response(..)` → reject connection-specific **and** any `te`.
   - `trailer(..)` → reject connection-specific **and** any `te` (trailers also may not contain
     pseudo-headers).
6. `into_response_parts()` → reject if `fields.contains_key("te")`.
7. New `into_trailer_parts(self) -> Result<HeaderMap, HeaderError>`:
   - Reject if any pseudo-header present (`self.pseudo.len() > 0`) → `PseudoInTrailer`.
   - Reject if `fields.contains_key("te")`.

### Gap 1 — call-site updates (map `HeaderError` → `StreamError` `H3_MESSAGE_ERROR`)
The call sites already do this; only signature changes are needed where constructors now return
`Result`:

- `h3/src/server/request.rs` (`resolve`): `Header::try_from` + `into_request_parts` already map to
  `StreamError::StreamError { code: H3_MESSAGE_ERROR }` and `stop_stream`/`stop_sending`. No
  behavioral change needed; the new `HeaderError` variants flow through the existing `Err` branch.
- `h3/src/client/stream.rs` (`recv_response`): `Header::try_from` + `into_response_parts` already
  map to `H3_MESSAGE_ERROR`. No change beyond the response constructor signature usage elsewhere.
- `h3/src/connection.rs` (trailers, `poll_recv_trailers`): switch `Header::try_from(...)?.into_fields()`
  to `Header::try_from(...)?.into_trailer_parts()`, mapping the new error to a `H3_MESSAGE_ERROR`
  stream error (mirrors upstream PR #364).
- `h3/src/server/stream.rs` (`send_response`): `Header::response(status, headers)` now returns
  `Result`; map error to `StreamError::StreamError { code: H3_MESSAGE_ERROR }`.
- `h3/src/connection.rs` (`send_trailers`): `Header::trailer(trailers)` now returns `Result`; map to
  `H3_MESSAGE_ERROR`.
- `h3/src/client/connection.rs` (`send_request`): `Header::request(...)` already handled with
  `.map_err(...)`; keep (it maps to `H3_INTERNAL_ERROR` today — see note in §6).
- Tests/helpers calling constructors: `qpack/decoder.rs`, `tests/request.rs` → add `.unwrap()` or
  handle the `Result`.

### Gap 2 — `h3/src/connection.rs`
- **Control stream:** already handled in `ConnectionInner::poll_control`
  (`Ok(None)` / `StreamTerminated` / `Unknown` → `H3_CLOSED_CRITICAL_STREAM`). No change.
- **QPACK encoder/decoder streams:** these are accepted into `encoder_recv`/`decoder_recv`
  (`Option<AcceptedRecvStream<..>>`) in `poll_accept_recv` but are **never polled**, so their
  closure is not currently detected. Add:
  - `ConnectionInner::poll_qpack_streams(&mut self, cx) -> Poll<Result<(), ConnectionError>>`:
    iterate the `encoder_recv`/`decoder_recv` streams, call `BufRecvStream::poll_data(cx)`; if it
    yields `Ok(None)` (FIN) or `Err(_)` (reset/terminated), set a closed flag; after the loop, if
    closed, return `Poll::Ready(Err(self.handle_connection_error(InternalConnectionError::new(
    Code::H3_CLOSED_CRITICAL_STREAM, "QPACK encoder or decoder stream was closed"))))`. Data
    (`Ok(Some(_))`) is discarded (stateless QPACK); `Poll::Pending` means stream still open.
  - Call `poll_qpack_streams` from `ConnectionInner::poll_control`, immediately after
    `poll_accept_recv`, and before the `control_recv` presence check so it is checked even before
    the control stream arrives.
- This runs for both client (`client/connection.rs::poll_close`) and server
  (`server/connection.rs::poll_accept_request_stream_internal`) since both drive
  `ConnectionInner::poll_control`.

---

## 4. Error-type handling chosen

- **Header/message problems (Gap 1):** represented as `HeaderError` at the parse layer, converted
  to `StreamError::StreamError { code: Code::H3_MESSAGE_ERROR, reason }` at the request/response/
  trailer read site. This is a **stream error**, never a connection error, per RFC 9114 §4.1.2.
- **Critical-stream closure (Gap 2):** represented as a **connection error** via
  `handle_connection_error` → `ConnectionError::Local { LocalError::Application { code:
  Code::H3_CLOSED_CRITICAL_STREAM, .. } }`, matching the existing control-stream path.

---

## 5. Test matrix

### Unit tests (in `h3/src/proto/headers.rs`)
Directly exercise the validation function on constructed `HeaderField` vectors:
| # | Input | Expected |
|---|-------|----------|
| U1 | Request with `Connection: keep-alive` | `Err(ConnectionSpecificHeader)` |
| U2 | Request with `Keep-Alive` / `Proxy-Connection` / `Transfer-Encoding` / `Upgrade` | `Err(ConnectionSpecificHeader)` |
| U3 | Request with `TE: trailers` | `Ok` (accepted) |
| U4 | Request with `TE: Trailers ` (whitespace/mixed-case) | `Ok` (accepted) |
| U5 | Request with `TE: chunked` | `Err(InvalidTeHeader)` |
| U6 | Response with `TE: trailers` | `Err` via `into_response_parts` |
| U7 | Trailer section with `TE` | `Err` via `into_trailer_parts` |
| U8 | Trailer section with a pseudo-header (`:status`) | `Err(PseudoInTrailer)` |
| U9 | Duplicate `Cookie` headers | concatenated `a=b; c=d; e=f` |
| U10 | Constructor `response`/`trailer` reject connection-specific fields | `Err(ConnectionSpecificHeader)` |

### Integration tests
- `tests/request.rs` / `tests/connection.rs` (use the `Pair` harness with real quinn):
  - **Req `Connection: keep-alive`** → server `resolve_request` returns
    `StreamError::StreamError { code: H3_MESSAGE_ERROR }`.
  - **Response `Upgrade: h3`** → client `recv_response` returns `StreamError` `H3_MESSAGE_ERROR`.
  - **Req `TE: trailers`** → accepted (server sees the request).
  - **Req `TE: chunked`** → server `StreamError` `H3_MESSAGE_ERROR`.
  - **Response `TE: trailers`** → client `StreamError` `H3_MESSAGE_ERROR`.
  - **Control-stream closure** → connection error `H3_CLOSED_CRITICAL_STREAM`
    (already covered by `control_close_send_error`; keep as-is).
  - **QPACK encoder-stream closure** → server `ConnectionError::Local {
    H3_CLOSED_CRITICAL_STREAM }` (new, `tests/connection.rs`).

> If any full connection-level test proves unreliable with the `Pair` harness, we keep the unit
> tests as the source of truth, keep whichever integration tests pass reliably, and document the
> gap in this plan / the final report.

---

## 6. Spec subtleties / judgment calls

1. **`H3_STREAM_CLOSED` vs `H3_CLOSED_CRITICAL_STREAM`:** used the RFC-correct
   `H3_CLOSED_CRITICAL_STREAM` (0x104); the task's `H3_STREAM_CLOSED` is not an RFC 9114 code and
   is inconsistent with the crate's existing control-stream handling.
2. **`TE` context sensitivity:** `Field::parse` cannot know request-vs-response, so it only checks
   the *value* (`trailers`); the response/trailer paths reject `te` outright. This keeps
   "TE allowed in requests, forbidden elsewhere" correct.
3. **Stateless QPACK:** the crate uses `decode_stateless` and does not maintain a QPACK dynamic
   table over the encoder/decoder streams; those streams carry only their stream-type header in
   practice. Polling them for closure (and discarding any bytes) is safe and does not disturb
   existing flows.
4. **Cookie concatenation location:** done during `TryFrom<Vec<HeaderField>>` (decode), so both
   requests and responses get it, and it happens before the message is handed to the application.
5. **`Header::request` in `client/connection.rs` maps to `H3_INTERNAL_ERROR`:** this is the
   *outgoing* (client-generated) request path. RFC 9114 says endpoints MUST NOT *generate*
   connection-specific fields, so rejecting at construction is correct. The existing call maps a
   construction failure to a connection error; we leave its existing mapping unchanged to avoid
   scope creep (documented, not altered).
6. **Pseudo-header cross-context (request pseudo in response):** handled structurally by the
   existing `Pseudo` design; the new work adds trailer pseudo-header rejection. Full cross-context
   pseudo validation is out of scope for this pass and matches upstream PR #364's scope.

---

## Internal review (reason-through before coding)

- **Is the single-choke-point approach sound?** Yes: both server request parsing
  (`server/request.rs`) and client response parsing (`client/stream.rs`) funnel decoded fields
  through `Header::try_from` → `Field::parse`, and both already translate `HeaderError` into a
  `H3_MESSAGE_ERROR` stream error. Adding the checks there guarantees both sides and satisfies the
  "never a connection error" requirement for Gap 1.
- **Is Gap 2 complete?** Control stream already covered. QPACK encoder/decoder streams are accepted
  but unpolled, so closure detection is genuinely missing — the new `poll_qpack_streams` addresses
  exactly that, and it is reachable from both client and server drivers.
- **Any borrow-checker traps?** `poll_qpack_streams` collects a closure flag inside the loop and
  calls `self.handle_connection_error` only *after* the loop to avoid a second mutable borrow of
  `self`. The `poll_control` restructuring moves `poll_accept_recv` up; all uses of `self` are
  sequential.
- **Will existing tests break?** Normal peers keep encoder/decoder streams open, so
  `poll_qpack_streams` stays `Pending`. Raw-quinn tests that open only control streams leave the
  encoder/decoder `Option`s `None`. Constructor signature changes (`response`/`trailer` → `Result`)
  require touching a handful of known call sites; all are enumerated in §3.