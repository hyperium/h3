# h3

An async HTTP/3 implementation.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/hyperium/h3/workflows/CI/badge.svg)](https://github.com/hyperium/h3/actions?query=workflow%3ACI)
[![Discord chat](https://img.shields.io/discord/500028886025895936.svg?logo=discord)](https://discord.gg/q5mVhMD)

This crate provides an [HTTP/3][spec] implementation that is generic over a provided QUIC transport. This allows the project to focus on just HTTP/3, while letting users pick their QUIC implementation based on their specific needs. It includes client and server APIs. Check the original [design][] for more details.

[spec]: https://www.rfc-editor.org/rfc/rfc9114
[design]: design/PROPOSAL.md

## Status

The `h3` crate is still very experimental. While the client and servers do work, there may still be bugs. And the API could change as we continue to explore. That said, we eagerly welcome contributions, trying it out in test environments, and using at your own risk.

The eventual goal is to use `h3` as an internal dependency of [hyper][].

[hyper]: https://hyper.rs

## Features

* HTTP/3 client and server implementation
* Async only API
* QUIC transport abstraction via traits in the [`quic`](https://github.com/hyperium/h3/h3/src/quic.rs) module
* The only supported QUIC implementation to date is [Quinn](https://github.com/quinn-rs/quinn)

## Overview

* **h3** HTTP/3 implementation
* **h3-quinn** QUIC transport implementation based on [Quinn](https://github.com/quinn-rs/quinn/)

## Getting Started

The [examples](./examples) directory can help get started in two ways:

- There are ready-to-use `client` and `server` binaries to interact with _other_ HTTP/3 peers. Check the README in that directory.
- The source code of those examples can help teach how to use `h3` as either a client or a server.

### Server

```rust
let (endpoint, mut incoming) = h3_quinn::quinn::Endpoint::server(server_config, "[::]:443".parse()?)?;

while let Some((req, stream)) = h3_conn.accept().await? {
   let resp = http::Response::builder().status(Status::OK).body(())?;
   stream.send_response(resp).await?;

   stream.send_data(Bytes::new("It works!")).await?;
   stream.finish().await?;
}

endpoint.wait_idle();
```

You can find a full server examples in [`examples/server.rs`](https://github.com/hyperium/h3/examples/server.rs)

### Client

``` rust
let addr: SocketAddr = "[::1]:443".parse()?;

let quic = h3_quinn::Connection::new(client_endpoint.connect(addr, "server")?.await?);
let (mut driver, mut send_request) = h3::client::new(quinn_conn).await?;

let drive = async move {
    future::poll_fn(|cx| driver.poll_close(cx)).await?;
    Ok::<(), Box<dyn std::error::Error>>(())
};

let request = async move {
    let req = http::Request::builder().uri(dest).body(())?;

    let mut stream = send_request.send_request(req).await?;
    stream.finish().await?;

    let resp = stream.recv_response().await?;

    while let Some(mut chunk) = stream.recv_data().await? {
        let mut out = tokio::io::stdout();
        out.write_all_buf(&mut chunk).await?;
        out.flush().await?;
    }
    Ok::<_, Box<dyn std::error::Error>>(())
};

let (req_res, drive_res) = tokio::join!(request, drive);
req_res?;
drive_res?;

client_endpoint.wait_idle().await;
```

You can find a full client example in [`examples/client.rs`](https://github.com/hyperium/h3/examples/client.rs)

## QUIC Generic

As mentioned, the goal of this library is to be generic over a QUIC implementation. To that effect, integrations with QUIC libraries exist:

- `h3-quinn`: in this same repository.
- [`s2n-quic-h3`](https://github.com/aws/s2n-quic/tree/main/quic/s2n-quic-h3)

## Interoperability

This crate as well as the quic implementation are [tested](https://github.com/quinn-rs/quinn-interop) for interoperability and performance in the [quic-interop-runner](https://github.com/marten-seemann/quic-interop-runner).
You can see the results at (https://interop.seemann.io/).

## License

h3 is provided under the MIT license. See [LICENSE](LICENSE).

