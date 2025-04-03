# h3-quinn

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](../LICENSE)
[![CI](https://github.com/hyperium/h3/workflows/CI/badge.svg)](https://github.com/hyperium/h3/actions?query=workflow%3ACI)
[![Crates.io](https://img.shields.io/crates/v/h3-quinn.svg)](https://crates.io/crates/h3-quinn)
[![Documentation](https://docs.rs/h3-quinn/badge.svg)](https://docs.rs/h3-quinn)

QUIC transport implementation for [h3](https://github.com/hyperium/h3) based on [Quinn](https://github.com/quinn-rs/quinn).

## Overview

`h3-quinn` provides the integration between the `h3` HTTP/3 implementation and the `quinn` QUIC transport library. This creates a fully functional HTTP/3 client and server using Quinn as the underlying QUIC implementation.

## Features

- Complete implementation of the `h3` QUIC transport traits
- Full support for HTTP/3 client and server functionality
- Optional tracing support
- Optional datagram support

## License

This project is licensed under the [MIT license](../LICENSE).

## See Also

- [h3](https://github.com/hyperium/h3) - The core HTTP/3 implementation
- [Quinn](https://github.com/quinn-rs/quinn) - The QUIC implementation used by this crate
