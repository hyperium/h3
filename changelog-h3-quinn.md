### v0.0.11 (2026-11-26)
* fix: stop_sending panic when recv stream is in-flight ([#331](https://github.com/hyperium/h3/pull/331))
* fix: allow checking recv_id while a read is pending ([#357](https://github.com/hyperium/h3/pull/357))
* fix: flush data buffered by send_data before finish ([#360](https://github.com/hyperium/h3/pull/360))
* feat: map QUIC ConnectionClosed to a typed error ([#336](https://github.com/hyperium/h3/pull/336))
* feat: expose 0-RTT detection at stream level ([#323](https://github.com/hyperium/h3/pull/323))
* update h3 to 0.1.0
* update h3-datagram to 0.0.3 (optional)

### v0.0.10 (2025-05-06)
* use new h3 error types
* use new datagram traits

### v0.0.9 (2025-03-18)
* fix private field usage from quinn

### v0.0.8 (2025-03-15)
* avoid copying data when sending
* new datagram feature

### v0.0.7 (2024-07-01)
* Consolidate quic trait redundancy
* make types Sync
* new tracing feature

### v0.0.6 (2024-05-20)
* use quinn 0.11

### v0.0.5 (2024-01-24)

* Update to `http` v1.

### v0.0.4 (2023-10-23)

* Fix `cargo doc` warning ([3ef7c1a](https://github.com/hyperium/h3/commit/3ef7c1a37b635e8446322d8f8d3a68580a208ad8))
* Initial WebTransport support ([22da938](https://github.com/hyperium/h3/commit/22da9387f19d724852b3bf1dfd7e66f0fd45cb81))


### v0.0.3 (2023-05-16)

* Update dependencies (quinn 0.10 and rustls 0.21) ([fabf614](https://github.com/hyperium/h3/commit/fabf6149c9ee57d7803ea5fb4426f895cbb5d244))

### v0.0.2 (2023-04-11)

#### Bug Fixes

* support quinn 0.9 ([49301f1](https://github.com/hyperium/h3/commit/49301f18e15d3acffc2a8d8bea1a8038c5f3fe6d))


### v0.0.1 (2023-03-09)

initial release
