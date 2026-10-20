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
  dropping it does not close the HTTP/3 connection.
- Dispatch incoming WT bidi/uni streams by session ID. Obtain one datagram
  reader from the driver and dispatch datagrams by their associated stream ID;
  independent per-session readers would compete for the same input.
- `session.open_bi()`, `session.open_uni()` and `session.send_datagram()` are
  bound to that session. `driver.shutdown(max_requests)` initiates HTTP/3
  GOAWAY; the caller still owns session tasks and their completion.

The driver does not spawn tasks or buffer streams for applications. The caller
owns concurrent resolvers, session admission, routing (including streams that
arrive before acceptance), session closure, and drain policy. Keep the driver
alive and polled while session work is active.
