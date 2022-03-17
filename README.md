# h3

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/hyperium/h3/workflows/CI/badge.svg)](https://github.com/hyperium/h3/actions?query=workflow%3ACI)
[![Discord chat](https://img.shields.io/discord/500028886025895936.svg?logo=discord)](https://discord.gg/q5mVhMD)

This crate provides an [HTTP/3][spec] implementation that is generic over a provided QUIC transport. This allows the project to focus on just HTTP/3, while letting users pick their QUIC implementation based on their specific needs. It includesclient and server APIs. Check the original [design][] for more details.

[spec]: https://quicwg.org/base-drafts/draft-ietf-quic-http.html
[design]: design/PROPOSAL.md

## Status

The `h3` crate is still very experimental. While the client and servers do work, there may still be bugs. And the API could change as we continue to explore. That said, we eagerly welcome contributions, trying it out in test environments, and using at your own risk.

The eventual goal is to use `h3` as an internal dependency of [hyper][].

[hyper]: https://hyper.rs

## Getting Started

The [examples][] directory can help get started in two ways:

- There are ready-to-use `client` and `server` binaries to interact with _other_ HTTP/3 peers. Check the README in that directory.
- The source code of those examples can help teach how to use `h3` as either a client or a server.

## QUIC Generic

As mentioned, the goal of this library is to be generic over a QUIC implementation. To that effect, integrations with QUIC libraries exist:

- `h3-quinn`: in this same repository.
- [`s2n-quic-h3`](https://github.com/aws/s2n-quic/tree/main/quic/s2n-quic-h3)


## License

h3 is provided under the MIT license. See [LICENSE](LICENSE).

