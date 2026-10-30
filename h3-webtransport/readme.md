# H3 Webtransport

Implementation of Webtransport protocol as extension of the h3 crate

# Status
This crate is still in experimental. The API is subject to change. It may contain bugs and is not yet complete. Use with caution.

## Connection and session ownership

The connection driver and WebTransport sessions have separate lifetimes:

- `WebTransportConnection::new(h3_connection)` retains the HTTP/3 connection.
  Keep calling `accept()` to process control traffic and receive both ordinary
  HTTP and WebTransport streams. Resolve each bidirectional stream independently
  so incomplete HEADERS do not block other requests.
- `accept_session(request, stream)` returns an owned acceptance future. Sending
  the CONNECT response does not borrow the driver. An accepted
  `WebTransportSession` holds its CONNECT stream and outgoing capabilities;
  dropping it does not close the HTTP/3 connection. `WebTransportSession::accept`
  takes this driver as well, so incoming streams always use the same reset registry.
- Dispatch incoming WT bidi/uni streams by session ID. Obtain one datagram
  reader from the driver and dispatch datagrams by their associated stream ID;
  independent per-session readers would compete for the same input.
- `session.open_bi()`, `session.open_uni()` and `session.send_datagram()` are
  bound to that session. `driver.shutdown(max_requests)` initiates HTTP/3
  GOAWAY; the caller still owns session tasks and their completion.
- `session.closed().await` returns a `SessionClose` containing the application
  code and UTF-8 reason from the CONNECT close capsule. A clean FIN without a
  capsule returns code zero and an empty reason; resets and connection failures
  return the transport error. Concurrent and repeated observers share the result.
- `session.close(code, reason).await` sends a close capsule followed by FIN.
  Reasons longer than 1024 bytes are rejected. Queued DATA and FIN progress
  survive cancellation: the connection driver or a later call resumes them.
  Sending a close capsule requires `B: From<bytes::Bytes>`.
- A terminated session resets both halves of its bidirectional data streams,
  stops incoming unidirectional streams, resets outgoing unidirectional streams,
  and rejects new stream/datagram sends. Split handles remain covered. Other
  sessions and ordinary HTTP requests on the connection are unaffected.
- Keep driving the connection to receive capsules, complete local close I/O,
  and reject data sent after a close capsule. Capsule parsing retains partial
  input across cancellation and skips unknown capsule payloads without buffering
  them. Drain application work before closing a session; GOAWAY alone does not
  terminate it.

The driver does not spawn tasks or buffer streams for applications. The caller
owns concurrent resolvers, session admission, routing (including streams that
arrive before acceptance), and the decision to drain and close a session. Keep
this driver alive and polled while session work is active.

Session stream handles share their transport state with the session's reset
registry. Each I/O poll holds a short synchronous lock; no lock is held across
`Pending`. This allows closure to reset an idle handle without a background task
or transport-specific reset-by-ID API. Split halves retain the shared transport;
finish the sending half explicitly to send FIN while keeping the receiver alive.
Other capsule extensions are separate from this session-close API.

## Data stream application errors

The inherent `send.reset(code)` and `recv.stop_sending(code)` methods accept
`u32` application codes. Bidirectional streams expose both methods, including
after splitting. They map codes into the HTTP/3 WebTransport application-error
range and skip reserved HTTP/3 codepoints. Resetting one direction does not
terminate the session or its other streams.

Tokio and futures I/O errors contain `error::StreamError` as their inner error.
Downcast `io::Error::get_ref()` to inspect `application_code()` and the original
`http3_code()`. A reserved value, protocol error such as `WT_SESSION_GONE`, or
connection failure has no application code; its transport cause is retained.

The `h3::quic` traits deliberately retain raw wire-code semantics. Consumers of
those traits can use `error::to_http3_code`, `error::from_http3_code`, or wrap a
received `StreamErrorIncoming` with `error::StreamError::from`. Codes inside
session close capsules stay unchanged; the stream-code mapping does not apply
to them.
