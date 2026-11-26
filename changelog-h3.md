### v0.1.0 (2026-11-26)
* **Breaking:** `quic::ConnectionErrorIncoming` gains a `ConnectionClosed` variant; QUIC backends exhaustively matching it must add an arm ([#336](https://github.com/hyperium/h3/pull/336))
* fix: correct behavior of standard CONNECT ([#322](https://github.com/hyperium/h3/pull/322))
* fix: properly report H3_NO_ERROR when remote terminates stream ([#315](https://github.com/hyperium/h3/pull/315))
* fix: apply backpressure before reading more frames ([#344](https://github.com/hyperium/h3/pull/344))
* fix: flush data buffered by send_data before the next write and before finish ([#360](https://github.com/hyperium/h3/pull/360))
* fix: release request tracking when resolver ends early ([#365](https://github.com/hyperium/h3/pull/365))
* fix: stop_sending panic when recv stream is in-flight ([#331](https://github.com/hyperium/h3/pull/331))
* feat: expose 0-RTT detection at stream level ([#323](https://github.com/hyperium/h3/pull/323))
* feat: map QUIC ConnectionClosed to a typed error ([#336](https://github.com/hyperium/h3/pull/336))
* feat: expose `qpack` module as unstable ([#311](https://github.com/hyperium/h3/pull/311))
* feat: Add WebSocket `:protocol` extension per RFC 9220 ([#236](https://github.com/hyperium/h3/pull/236))
* feat: Add connect-ip (RFC9484) as a known HTTP/3 CONNECT protocol ([#273](https://github.com/hyperium/h3/pull/273))
* chore: reduce excess GREASE ([#362](https://github.com/hyperium/h3/pull/362))
* chore: trim unused dependencies, replace `futures` with `futures-util` ([#324](https://github.com/hyperium/h3/pull/324))
* chore: update dependencies ([#318](https://github.com/hyperium/h3/pull/318))
* docs: document more duvet compliance ([#359](https://github.com/hyperium/h3/pull/359))
* docs: update documentation of recv_data/poll_recv_data ([#343](https://github.com/hyperium/h3/pull/343))

### v0.0.8 (2025-05-06)
* fix integer overflow when parsing qpack prefixed integers
* introduce new user facing error types 
* introduce new quic traits facing error types
* `server::Connection::accept` now returns a  `RequestResolver` instead of direct resolving the request to avoid head of line blocking
* h3-datagram traits cleanup
* some fixes in error handling 

### v0.0.7 (2025-03-13)
* Expose poll_recv_trailers APIs
* Avoiding extra allocation for shared error
* Added .id() for client RequestStream
* move datagram to separate crate
* Client ability to stop streams with error code
* Add extended CONNECT setting for client conn

### v0.0.6 (2024-07-01)
* Consolidate quic trait redundancy
* start qpack streams 
* send grease stream in the background
* new tracing feature

### v0.0.5 (2024-05-20)
* add `poll_recv_data()` for server
* use 2021 edition
* some cleanups

### v0.0.4 (2024-01-24)

* Update to `http` v1.0
* Fix `try_recv` potentially hanging
* Fix prefixed integers on 32bit targets

### v0.0.3 (2023-10-23)

* Split out a `Settings` struct from `Config` ([a57ed22](https://github.com/hyperium/h3/commit/a57ed224ac5d17a635eb71eb6f83c1196f581a51))
* Add a test-only send_settings config option ([3991dca](https://github.com/hyperium/h3/commit/3991dcaf3801595e49d0bb7fb1649b4cf50292b7))
* Expose setting to disable grease ([dccb3cd](https://github.com/hyperium/h3/commit/dccb3cdae9d5a9d720fae5f774b53f0bd8a16019))
* bugfix: Actually encode extensions in header ([a38b194](https://github.com/hyperium/h3/commit/a38b194a2f00dc0b2b60564c299093204d349d7e))
* Initial support for RFC 9298 "Proxying UDP in HTTP" ([5a87580](https://github.com/hyperium/h3/commit/5a87580bd060b6a7d4dc625e990526d6998fda5c))
* Bump H3_DATAGRAM setting ID according to RFC9297 ([58c8e5c](https://github.com/hyperium/h3/commit/58c8e5cecb2b0c367d738989fe9c505936bc5ff3))
* Fix `cargo doc` warning ([3ef7c1a](https://github.com/hyperium/h3/commit/3ef7c1a37b635e8446322d8f8d3a68580a208ad8))
* Initial WebTransport support (in h3 is just some necessary code to support a WebTransport crate which contains most of the WebTransport implementation) ([22da938](https://github.com/hyperium/h3/commit/22da9387f19d724852b3bf1dfd7e66f0fd45cb81))


### v0.0.2 (2023-04-11)

#### Bug Fixes

* distinguish push and stream ids ([da29aea](https://github.com/hyperium/h3/commit/da29aea305d61146664189346b3718458cb9f4d6))


### v0.0.1 (2023-03-09)

initial release
