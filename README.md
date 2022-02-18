# H3

An async HTTP/3 implementation.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Build status](https://github.com/hyperium/h3/workflows/CI/badge.svg)](https://github.com/hyperium/h3/actions?query=workflow%3ACI)

## Features

* HTTP/3 client and server implementation
* Async only API
* QUIC transport abstraction via traits in the [`quic`](https://github.com/hyperium/h3/h3/src/quic.rs) module
* The only supported QUIC implementation to date is [Quinn](https://github.com/quinn-rs/quinn)

## Overview

* **h3** HTTP/3 implementation
* **h3-quinn** QUIC transport implementation based on [Quinn](https://github.com/quinn-rs/quinn/)

## Hyper integration

## Quick API overview

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

## QUIC implementation adaptors

### h3-quinn

### Writing your own

## Interoperability

This crate as well as the quic implemtation are tested for interoperabilty and performance in the [quic-interop-tester](TODO). You can see the results at (matren-seema.io) TODO

## Contributing
