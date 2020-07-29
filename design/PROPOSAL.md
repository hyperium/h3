# hyper + HTTP/3

This describes an effort to provide HTTP/3 support for the [hyper.rs](http://hyper.rs/) community.


## 1. Goals

What would be the goals of this effort, and what wouldn’t be.

- Provide a standalone HTTP/3 crate.
    - This is similar to what we did with `h2`. This allows folks that want more control over HTTP/3 specifically to be able to do so. Or people may wish to reduce their dependencies to the bare minimum, and thus don’t want to include crates supporting HTTP/1 and 2 (like `hyper` does).
    - We have the `h3` crate name already reserved, as one option.
    - It shouldn’t decide on synchronization or runtime needs.
- Allow users to provide their own QUIC implementation.
    - There are several QUIC implementations already, and new ones could appear in kernels instead of user-space. 
    - Implementing QUIC also requires picking a TLS implementation. Various adopters may have different requirements for which TLS library they use.
- Simple, optional integration in the `hyper` crate.
- Performance and correctness are paramount.
    - We should have a good way to frequently measure performance.
- Collaborative with the community.
    - All current HTTP/3 Rust implementations have hard dependencies on their QUIC implementations. By making `h3` generic over any QUIC-like interface, we allow people to plug in to existing stacks. Meanwhile, `h3` providing the HTTP/3-level details can focus effort and attention.
    - The hyper brand has recognition as an open source HTTP project.
        - Using the `h3` crate name removes some confusion about if it is specifically tied a certain QUIC library.


## 2. Overview

### Briefly, HTTP/3

HTTP/3 is a new networking protocol that defines HTTP semantics specifically over the QUIC transport. Similar to HTTP/2, it uses a binary framing, but the multiplexing property is pushed down into the transport layer (QUIC), which solves head-of-line blocking that still exists in HTTP/2 over TCP. It also uses a modified header compression strategy, similar to HPACK, but designed around QUIC's lack of in-order guarantee.

### The Proposed Stack

```text
                +--------------------------------+
                |                                |
                |                                |
                |        hyper::proto::h3        |
                |                                |
                |                                |
                +--------------------------------+

                +--------------------------------+
                |                                |
                |                                |
            ->  |               h3               |  <-
                |                                |
                |                                |
                +--------------------------------+

                +--------------------------------+
                |                                |
                |                                |
                |              QUIC              |
                |                                |
                |                                |
                +--------------------------------+

```

- **hyper::proto::h3** - The module integrating hyper with the `h3` crate. This includes connection management, runtime integration, `Service` and `HttpBody` usage.
- **h3** - The bulk of this proposal. This implements HTTP/3 data structures and futures, without managing connections or spawning tasks.
- **QUIC** - The underlying transport library. This proposal assumes other libraries will implement QUIC. In Section 5, this proposal outlines how to plug `h3` together with different QUIC libraries.


## 3. Features

- **Server** to accept requests and send responses
- **Client** to send requests and receive responses
- **Streaming** of message bodies
- **Trailers**
- **Graceful shutdown** and **idle shutdown**

### Non-goals

- **Synchronization**: As much as possible, synchronization should not be an internal feature.
    - We’ve learned from building the `h2` crate, which includes a `Mutex` around the stream store and buffer, and many different stream handles. This adds contention, and makes it increasingly difficult to realize when a stream is no longer reachable.
- **Runtime integration**: There shouldn’t be a dependency on any runtime. By not being opinionated about the runtime, a core library like an HTTP/3 state machine can see more adoption.

### Eventually

- **QPACK dynamic table**: the dynamic table is a performance optimization, but has the potential to block streams waiting on dynamic table updates. To reduce initial complexity, and to not require figuring out the correct heuristic, we can delay integrating the dynamic table till later. HTTP/3 even defaults to expecting zero space in the dynamic table unless the peer opts-in.
- **Server Push**
- **HTTP CONNECT** to tunnel over a single QUIC stream
- **Prioritization**: Stream priorities were defined in HTTP/2, and are especially useful for browsers to improve page load speeds. However, they are not specifically defined in QUIC or HTTP/3. There is a [different proposal][http-prio] that we should consider once stabilized.

[http-prio]: https://tools.ietf.org/html/draft-ietf-httpbis-priority-01


## 4. Public API

The API can be mostly split in two, the `client` and `server` modules. Check out Section 5 about transports.

### `mod server`

A `struct Connection` is the main type used here. After creating some sort of QUIC endpoint listener, and optionally with some configure, it would be used to create a `Connection`.

- `server::handshake(connection)` - An async function that uses default config, and sets up the required streams to manage an HTTP/3 connection (control streams, qpack), returning a `server::Connection`.
- `Connection::builder().max_field_section_size(1024 * 32).handshake(connection)` - A builder can change the configuration, and then does the same handshake.

After created, a server connection can be used to accept requests:

```rust
impl<T: quic::Connection<B>, B: Buf> Connection<T, B> {
    pub async fn accept(&mut self)
        -> Result<(Request<()>, RequestStream<T::BidiStream, B>), Error>;
}
```

The `http::Request` and a `RequestStream` are provided as when "accepted".

Since this uses the `BidiStream` of the `Connection`, the `RequestStream` is a single type that provides methods to receive request body and trailers, and to send a response, response body, and trailers. 

```rust
impl<T: quic::RecvStream, B> RequestStream<T, B> {
    /// Receive some of the request body.
    pub async fn recv_data(&mut self) -> Result<Option<T::Buf>, Error>;

    /// Receive an optional set of trailers for the request.
    pub async fn recv_trailers(&mut self) -> Result<Option<HeaderMap>, Error>;
}

impl<T: quic::SendStream<B>, B: Buf> RequestStream<T, B> {
    /// Send a response for the request.
    pub async fn send_response(&mut self: Response<()>) -> Result<(), Error>;
    
    /// Send some data on the response body.
    pub async fn send_data(&mut self, buf: B) -> Result<(), Error>;
    
    /// Send a set of trailers to end the response.
    pub async fn send_trailers(&mut self, trailers: HeaderMap) -> Result<(), Error>;
}
```

For QUIC transports that provide the ability to “split” the stream into two halves, the `RequestStream` can support that. This is beneficial to support users or APIs that would prefer to handle both sides on separate tasks, or as separate arguments.

```rust
impl<T: quic::BidiStream<B>, B> RequestStream<T, B> {
    /// Split this "bidi" `RequestStream` into two sides, one with
    /// a `SendStream` and the other with a `RecvStream`.
    pub fn split(self)
       -> (RequestStream<T::SendStream<B>, B>, RequestStream<T::RecvStream, B>);
}
```

**Alternative:** It *might* be better to `split` into distinct specialized types `RecvStream` and `SendResponse`. The reason to consider the alternative is in case the stream halves from `BidiStream::split` happen to be the same type as the non-split stream, then you could split multiple times. In practice, that seems unlikely.

By *not* having distinct specialized types, there’s no need to duplicate the APIs on multiple types (and avoids “What’s the different between `RequestStream<SendStream, B>` and `SendResponse<SendStream, B>`).


### `mod client`

**NOTE:** I have punted on building out an API for the `client`. However, much of the design is similar, with the major change being that requests are *sent* instead of *accepted*.

A `struct Connection` would be the main type used. Constructing one requires a QUIC transport, and optionally some configuration.

- `client::handshake(connection)`
- `Connection::builder().max_field_section_size(1024 * 64).handshake(connection)`

A `client::Connection` can then be used to send requests:

```rust
impl<T: quic::Connection<B>, B: Buf> Connection<T, B> {
    /// Polls for when the client can send a new request.
    pub fn poll_ready(&mut self, cx: &mut Context) -> Poll<Result<(), Error>>;
    
    pub fn send_request(&mut self, req: http::Request<()>)
        -> Result<client::RequestStream<T::BidiStream, B>, Error>;
}
```

It may be worth sketching out how the `BidiStream::split()` can be used to have ergonomic (but opt-in) split sides for sending data and awaiting the response.

## 5. QUIC Transport

To allow being generic over QUIC implementations, there should be a trait interface that users can fill in to provide their own transport layer. This is similar to how `h2` is generic over its transport via the `AsyncRead` and `AsyncWrite` traits.

This part of the API can be contained in a module, as opposed to the top level, since integrating a transport should be a one-and-done thing. Proposed module names:


- `quic` (assumed in this document)
- `transport`

Since establishing and accepting QUIC connections is out of scope for the crate, these concepts are not built into `h3`. A user could create their QUIC implementation’s concept of a `Connection`, and then pass that to `h3`. This is a similar design to `h2`, which asks users to do any transport building before handing it to `server::handshake(conn)` or `client::handshake(conn)`.

Some **unresolved questions** related to these traits are:


- How might a user ask for transport related properties, like the connection or stream IDs?
    - One way is to provide `stream()` and `stream_mut()` methods on the public-api `RequestStream` types, so users can collect library-specific data about the stream.

### Connections

Regardless of the peer’s “role” (client or server), there are similar requirements of the connection.


- They must be able to create unidirectional (send) streams.
    - The control stream
    - QPACK
    - Server’s can send push streams
- They must be able to accept unidirectional (receive) streams.
    - The remote’s control stream
    - The remote’s QPACK streams
    - Clients can accept push streams
- For now, only **clients** can *create* bidirectional (send and receive) streams, to send a request and receive a response.
- For now, only **servers** can *accept* bidirectional (send and receive) streams.

We could handle the differences between “roles” in a few ways:

- There can be a single `Connection` trait, and any methods a role cannot fulfill could simply return an error when called.
    - For example, a server could implement `poll_open_bidirectional_stream` to return `H3_STREAM_CREATION_ERROR`.
- There could be a base `Connection` trait of the common functionality, and the `Client` and `Server` traits that include their specific methods.

```rust
trait Connection<B: Buf> {
    type SendStream: SendStream<B>;
    type RecvStream: RecvStream;
    type BidiStream: SendStream<B> + RecvStream;
    type Error;
    
    // Accepting streams
 
    fn poll_accept_bidirectional_stream(self: Pin<&mut Self>, cx: &mut Context)
        -> Poll<Result<Option<Self::BidiStream>, Self::Error>>;
           
    fn poll_accept_recv_stream(self: Pin<&mut Self>, cx: &mut Context)
        -> Poll<Result<Option<Self::RecvStream>, Self::Error>>;
        
    // Creating streams
    
    fn poll_open_bidirectional_stream(self: Pin<&mut Self>, cx: &mut Context)
        -> Poll<Result<Self::BidiStream, Self::Error>>;

    fn poll_open_send_stream(self: Pin<&mut Self>, cx: &mut Context)
        -> Poll<Result<Self::SendStream, Self::Error>>;
}
```

### Streams

Streams over a single connection can be used to send data, receive data, or both. There a few ways we can allow plugging in generic support for streams:


- `futures::Stream` and `futures::Sink`
- `io::AsyncRead` and `io::AsyncWrite`
- Defining our own custom `SendStream` and `ReceiveStream` traits
    - We can add new methods (with default implementations) if we control the traits.
    - The trait names can be clearer (`h3::transport::SendStream` vs `Stream<Item = Result<Bytes, Error>>`)
    - It can cause more boilerplate when integrating with other libraries that may provide their streams implemented with the more generic traits
        - To compensate, we can provide `wrap_stream` or `wrap_io` versions


Proposed Streams traits:


```rust
trait SendStream<B: Buf> {
    type Error; // bounds?
    
    /// Polls that the stream is ready to send more data.
    // Q: Should this be Pin<&mut Self>?
    fn poll_ready(&mut self, cx: &mut Context) -> Poll<Result<(), Self::Error>>;
    
    fn send_data(&mut self, data: B) -> Result<(), Self::Error>;
    
    // fn poll_flush?
    
    fn poll_finish(&mut self, cx: &mut Context) -> Poll<Result<(), Self::Error>>;
    
    fn reset(&mut self, reset_code: u64);
}
```

```rust
trait RecvStream {
    type Buf: Buf;
    type Error; // bounds?
    
    // Poll for more data received from the remote on this stream.
    fn poll_data(&mut self, cx: &mut Context)
       -> Poll<Result<Option<Self::Buf>, Self::Error>>;
       
    fn stop_sending(&mut self, error_code: u64);
}
```

The `Connection` also have methods to accept and open “bidirectional” streams. These are single types that implement both `SendStream` and `RecvStream`. However, some libraries will already have the internal synchronization to allow for these types to be “split” into a send and a receive half. To allow for opting into “splitting” streams, there’s a 3rd optional stream trait:

```rust
trait BidiStream<B>: SendStream<B> + RecvStream {
    type SendStream: SendStream<B>;
    type RecvStream: RecvStream;
    
    fn split(self) -> (Self::SendStream, Self::RecvStream);
}
```

**Bonus:** we can also provide a `quic::split` utility that provides wrapper types to implement `BidiStream` by wrapping the inner type in a bi-lock. This is entirely for convenience,

## 6. Security Considerations

The HTTP/3 draft mentions several [security considerations](https://quicwg.org/base-drafts/draft-ietf-quic-http.html#name-security-considerations), this explains how each are handled.


1. **Server authority**: deciding if a server should accept a given authority is punted to the user of the library.
2. **Cross-Protocol Attacks**: Use of TLS ALPN and QUIC as the transport layer takes care of this.
3. **Intermediary Encapsulation Attacks**: HTTP/3 field encoding allows for illegal character in headers. The usage of `http::HeaderName` to forbid illegal HTTP/1 or 2 characters protects us.
4. **Cacheability of Pushed Responses**: Caching is punted to the user of the library.
5. **Denial of Service**
    1. **Limits on field section size**: The `SETTINGS_MAX_HEADER_LIST_SIZE` value can be configured by users on both the client and server `Builder`s, and we can default to a “typical” safe size.
    2. **CONNECT Issues**: Establishing of `CONNECT` tunnels is punted to the user of the library.
6. **Use of compression**: Usage of `http::HeaderValue`‘s `sensitive` flag allows users to protect secrets from compression. Choosing what values to mark `sensitive` is punted to the user of the library.
7. **Padding and Traffic Analysis**: We won’t start with any use of HTTP/3 padding. If needed, we can add `Builder` options to enable various padding strategies.
8. **Frame Parsing**: We must use care with var-int length decoding. We should make sure our fuzzing coverage involves fuzzing HTTP/3 framing.
9. **Early Data**: Using QUIC 0rtt allows replay attacks. Users of the library should not enable 0rtt without anti-replay mechanisms in place.
10. **Migration**: The clients address may change over time. We should be careful about caching a client’s `SocketAddr`, and users of the library should as well.

## 7. Testing

The correctness of the HTTP/3 implementation is supremely important. We will need a testing strategy that ensures the implementation is correct while developing, and to catch regressions.


- **Unit Tests: **Each module should have suitable unit tests for invariants it upholds.
- **Integration Tests: **Testing the public API of the `client` and `server`. This includes being to simulate incorrect or bad-acting peers.
- **Interoperability Tests:** It’s helpful to test our library with others, to validate any edge cases or timing differences that exist.
- **Fuzzing:** fuzzing input that would be received from the network is especially important, to ensure we don’t do anything wrong, or panic.
    - QPACK decoding
    - HTTP/3 Frame decoding

### Benchmarks

The performance of HTTP/3 is also critical. We need to setup both unit benchmarks and integration benchmarks to ensure the implementation is at least as fast as existing solutions, and that changes do no regress.


- **Unit benchmarks**: This includes things like QPACK encoding/decoding, HTTP/3 frame encoding/decoding.
- **Integration benchmarks**: This involves creating a client and server and sending different loads, measuring both requests per second, and throughput.
    - Being able to configure benchmarks that simulate latency would be incredible helpful.

## 8. Alternatives

- **Use an existing Rust HTTP/3 library.**
    - The existing options (quiche-h3, neqo-h3, and quinn-h3) have some downsides.
    - neqo and quiche are designed to be ignorant of IO, and are simple state machines. While this provides maximum flexibility, it still requires significant complexity for each user to plug into their application.
    - All 3 of them have hard dependencies on their QUIC implementations, which conflicts with one of our main goals to allow users to bring their own QUIC implementation.
- **Fork an existing Rust HTTP/3 library, probably quinn-h3.**
    - This can skip the need to get consensus with others, but also is somewhat "hostile".
    - This would still require changing the code a fair amount to cover the requirements in this document.
    - It would reduce the amount of overall contributors, and does the opposite of "earning trust" in the OSS community.
