use std::sync::Arc;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures_util::FutureExt;
use h3::{
    ext::Protocol,
    proto::{
        coding::Encode,
        frame::{Frame, SettingId, Settings},
        headers::Header,
    },
    stream::{BidiStreamHeader, UniStreamHeader},
};
use h3_datagram::datagram::Datagram;
use h3_webtransport::server::{AcceptedBi, IncomingStream, WebTransportConnection};
use http::{Method, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Driver = WebTransportConnection<h3_quinn::Connection, Bytes>;

// A raw peer lets the regression keep a HEADERS frame incomplete while sending
// other requests. No timers or background polling tasks coordinate this test.
async fn pair() -> (
    quinn::Endpoint,
    quinn::Endpoint,
    quinn::Connection,
    Driver,
    quinn::SendStream,
) {
    pair_with_limits(None, None).await
}

async fn pair_with_limits(
    window: Option<u32>,
    bidi_limit: Option<u32>,
) -> (
    quinn::Endpoint,
    quinn::Endpoint,
    quinn::Connection,
    Driver,
    quinn::SendStream,
) {
    let cert = rcgen::generate_simple_self_signed(vec!["example.test".into()]).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
    server_tls.alpn_protocols = vec![b"h3".to_vec()];
    let server = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_tls).unwrap(),
        )),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.cert.der().clone()).unwrap();
    let mut client_tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let mut client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
    ));
    let mut transport = quinn::TransportConfig::default();
    if let Some(window) = window {
        transport.stream_receive_window(window.into());
    }
    if let Some(limit) = bidi_limit {
        transport.max_concurrent_bidi_streams(limit.into());
    }
    client_config.transport_config(Arc::new(transport));
    client.set_default_client_config(client_config);
    let connect = client
        .connect(server.local_addr().unwrap(), "example.test")
        .unwrap();
    let (peer, accepted) = tokio::join!(connect, async {
        server.accept().await.unwrap().await.unwrap()
    });
    let peer = peer.unwrap();
    let mut control = peer.open_uni().await.unwrap();
    let mut settings = Settings::default();
    settings.insert(SettingId::ENABLE_WEBTRANSPORT, 1).unwrap();
    settings.insert(SettingId::H3_DATAGRAM, 1).unwrap();
    settings
        .insert(SettingId::ENABLE_CONNECT_PROTOCOL, 1)
        .unwrap();
    let mut wire = BytesMut::new();
    wire.put_u8(0); // HTTP/3 control stream type.
    Frame::<Bytes>::Settings(settings).encode(&mut wire);
    control.write_all(&wire).await.unwrap();
    let conn = h3::server::builder()
        .enable_webtransport(true)
        .enable_datagram(true)
        .enable_extended_connect(true)
        .max_webtransport_sessions(2)
        .build(h3_quinn::Connection::new(accepted))
        .await
        .unwrap();
    (server, client, peer, Driver::new(conn), control)
}

fn headers(connect: bool) -> Bytes {
    let mut req = Request::builder()
        .method(if connect {
            Method::CONNECT
        } else {
            Method::GET
        })
        .uri("https://example.test/")
        .body(())
        .unwrap();
    if connect {
        req.extensions_mut().insert(Protocol::WEB_TRANSPORT);
    }
    let (parts, ()) = req.into_parts();
    let header = Header::request(parts.method, parts.uri, parts.headers, parts.extensions).unwrap();
    let mut block = BytesMut::new();
    h3::qpack::encode_stateless(&mut block, header).unwrap();
    let block = block.freeze();
    let mut wire = BytesMut::new();
    Frame::<Bytes>::Headers(block.clone()).encode(&mut wire);
    wire.extend_from_slice(&block);
    wire.freeze()
}

async fn request(
    peer: &quinn::Connection,
    connect: bool,
) -> (quinn::SendStream, quinn::RecvStream) {
    let (mut send, recv) = peer.open_bi().await.unwrap();
    send.write_all(&headers(connect)).await.unwrap();
    (send, recv)
}

async fn resolver(
    driver: &mut Driver,
) -> h3_webtransport::server::BiStreamResolver<h3_quinn::Connection, Bytes> {
    match driver.accept().await.unwrap().unwrap() {
        IncomingStream::Bidirectional(resolver) => resolver,
        _ => panic!("expected a bidirectional stream"),
    }
}

async fn resolved_request(
    driver: &mut Driver,
) -> (
    Request<()>,
    h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) {
    match resolver(driver).await.resolve().await.unwrap() {
        AcceptedBi::Request(request, stream) => (request, stream),
        _ => panic!("expected HTTP headers"),
    }
}

#[tokio::test]
async fn sessions_do_not_take_the_driver_or_discard_pending_headers() {
    let (_server, _client, peer, mut driver, _control) = pair().await;

    let wire = headers(false);
    let (mut slow, _slow_recv) = peer.open_bi().await.unwrap();
    slow.write_all(&wire[..1]).await.unwrap();
    let mut pending_headers = Box::pin(resolver(&mut driver).await.resolve());
    assert!(pending_headers.as_mut().now_or_never().is_none());

    let (_connect, _connect_recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let accepting = driver.accept_session(req, stream);

    // This compiles and progresses even before the session acceptance future
    // is polled: response I/O does not borrow or consume the connection driver.
    let (_get, _get_recv) = request(&peer, false).await;
    let (req, mut stream) = resolved_request(&mut driver).await;
    assert_eq!(req.method(), Method::GET);
    stream.send_response(Response::new(())).await.unwrap();
    stream.finish().await.unwrap();
    let session = accepting.await.unwrap();

    let (_connect2, _connect_recv2) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let second = driver.accept_session(req, stream).await.unwrap();
    assert_ne!(session.session_id(), second.session_id());
    drop(session);

    slow.write_all(&wire[1..]).await.unwrap();
    match pending_headers.await.unwrap() {
        AcceptedBi::Request(req, mut stream) => {
            assert_eq!(req.method(), Method::GET);
            stream.send_response(Response::new(())).await.unwrap();
            stream.finish().await.unwrap();
        }
        _ => panic!("pending HTTP request lost across session acceptance"),
    }

    let (_get, _recv) = request(&peer, false).await;
    let (_, mut stream) = resolved_request(&mut driver).await;
    stream.send_response(Response::new(())).await.unwrap();
    stream.finish().await.unwrap();
    // An error-free request after dropping the first session also proves that
    // its drop did not close the connection or the second session's transport.
    second.send_datagram(Bytes::from_static(b"alive")).unwrap();
    let datagram = Datagram::decode(peer.read_datagram().await.unwrap()).unwrap();
    assert_eq!(
        h3_webtransport::SessionId::from(datagram.stream_id()),
        second.session_id()
    );
    assert_eq!(datagram.payload(), &Bytes::from_static(b"alive"));
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn rejected_and_cancelled_acceptance_leave_the_driver_usable() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_get, mut response) = request(&peer, false).await;
    let (req, stream) = resolved_request(&mut driver).await;
    assert!(driver.accept_session(req, stream).await.is_err());
    assert!(!response.read_to_end(1024).await.unwrap().is_empty());

    let (_connect, _recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    drop(driver.accept_session(req, stream));

    let (_get, _recv) = request(&peer, false).await;
    let (req, mut stream) = resolved_request(&mut driver).await;
    assert_eq!(req.method(), Method::GET);
    stream.send_response(Response::new(())).await.unwrap();
    stream.finish().await.unwrap();
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn driver_routes_streams_and_datagrams_while_session_remains_separate() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let id = session.session_id();

    let (mut send, mut recv) = peer.open_bi().await.unwrap();
    let mut wire = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(id).encode(&mut wire);
    wire.extend_from_slice(b"bidi");
    send.write_all(&wire).await.unwrap();
    send.finish().unwrap();
    let mut stream = match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(actual, stream) => {
            assert_eq!(actual, id);
            stream
        }
        _ => panic!("expected WT stream"),
    };
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.unwrap();
    assert_eq!(body, b"bidi");

    let mut uni = peer.open_uni().await.unwrap();
    wire.clear();
    UniStreamHeader::WebTransportUni(id).encode(&mut wire);
    wire.extend_from_slice(b"uni");
    uni.write_all(&wire).await.unwrap();
    uni.finish().unwrap();
    match driver.accept().await.unwrap().unwrap() {
        IncomingStream::Unidirectional(actual, mut incoming) => {
            assert_eq!(actual, id);
            body.clear();
            incoming.read_to_end(&mut body).await.unwrap();
            assert_eq!(body, b"uni");
        }
        _ => panic!("expected WT uni stream"),
    }

    let mut datagrams = driver.datagram_reader();
    let datagram = Datagram::new(
        h3::quic::StreamId::try_from(0).unwrap(),
        Bytes::from_static(b"incoming"),
    );
    let mut encoded = datagram.encode();
    let encoded = encoded.copy_to_bytes(encoded.remaining());
    peer.send_datagram(encoded).unwrap();
    let incoming = datagrams.read_datagram().await.unwrap();
    assert_eq!(h3_webtransport::SessionId::from(incoming.stream_id()), id);
    assert_eq!(incoming.payload(), &Bytes::from_static(b"incoming"));

    driver.shutdown(0).await.unwrap();
    // GOAWAY is not termination of already admitted stream work.
    stream.write_all(b"after GOAWAY").await.unwrap();
    stream.shutdown().await.unwrap();
    assert_eq!(recv.read_to_end(1024).await.unwrap(), b"after GOAWAY");
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn connect_fin_is_cancel_safe_and_resets_admitted_work() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, _recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    assert!(session.closed().now_or_never().is_none());

    // Cancelling the observer partway through a DATA frame retains the reader.
    let mut data = BytesMut::new();
    let payload = Bytes::from_static(b"\x17\x00"); // Unknown capsule, zero-length payload.
    Frame::Data(payload.clone()).encode(&mut data);
    data.extend_from_slice(&payload);
    connect.write_all(&data[..3]).await.unwrap();
    assert!(session.closed().now_or_never().is_none());
    connect.write_all(&data[3..]).await.unwrap();

    let (mut send, mut recv) = peer.open_bi().await.unwrap();
    let mut header = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(session.session_id()).encode(&mut header);
    send.write_all(&header).await.unwrap();
    let mut admitted = match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(_, stream) => stream,
        _ => panic!("expected WT stream"),
    };

    driver.shutdown(0).await.unwrap();
    assert!(session.closed().now_or_never().is_none());
    connect.finish().unwrap();
    let (first, second) = tokio::join!(session.closed(), session.closed());
    first.unwrap();
    second.unwrap();
    session.closed().await.unwrap();

    assert_eq!(session.closed().await.unwrap(), Default::default());
    assert!(admitted.write_all(b"after FIN").await.is_err());
    assert_reset(recv.read_to_end(1024).await.unwrap_err());
    assert_eq!(
        send.stopped().await.unwrap().unwrap().into_inner(),
        SESSION_GONE
    );
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn connect_reset_is_retained_without_closing_sibling_sessions_or_http() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, _recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let (_connect2, _recv2) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let sibling = driver.accept_session(req, stream).await.unwrap();

    let observing = session.closed();
    tokio::pin!(observing);
    assert!(observing.as_mut().now_or_never().is_none());
    session
        .send_datagram(Bytes::from_static(b"while waiting"))
        .unwrap();
    let datagram = Datagram::decode(peer.read_datagram().await.unwrap()).unwrap();
    assert_eq!(
        h3_webtransport::SessionId::from(datagram.stream_id()),
        session.session_id()
    );

    connect.reset(0x52u32.into()).unwrap();
    let (first, second) = tokio::join!(observing, session.closed());
    let first = first.unwrap_err();
    assert!(
        matches!(&*first, h3::error::StreamError::RemoteTerminate { code } if *code == h3::error::Code::from(0x52))
    );
    assert!(Arc::ptr_eq(&first, &second.unwrap_err()));
    assert!(Arc::ptr_eq(&first, &session.closed().await.unwrap_err()));
    assert!(sibling.closed().now_or_never().is_none());

    let (_get, mut recv) = request(&peer, false).await;
    let (_, mut stream) = resolved_request(&mut driver).await;
    stream.send_response(Response::new(())).await.unwrap();
    stream.finish().await.unwrap();
    assert!(!recv.read_to_end(1024).await.unwrap().is_empty());
    sibling
        .send_datagram(Bytes::from_static(b"still alive"))
        .unwrap();
    let datagram = Datagram::decode(peer.read_datagram().await.unwrap()).unwrap();
    assert_eq!(
        h3_webtransport::SessionId::from(datagram.stream_id()),
        sibling.session_id()
    );
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn connection_loss_wakes_connect_observer_beside_the_driver() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _recv) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    assert!(session.closed().now_or_never().is_none());
    peer.close(0u32.into(), b"done");
    let (closed, _driver_end) = tokio::join!(session.closed(), driver.accept());
    assert!(matches!(
        &*closed.unwrap_err(),
        h3::error::StreamError::ConnectionError(_)
    ));
}

const SESSION_GONE: u64 = 0x170d7b68;

fn close_capsule(code: u32, reason: &str) -> Bytes {
    use h3::proto::varint::VarInt;
    let mut payload = BytesMut::new();
    VarInt::from_u32(0x2843).encode(&mut payload);
    VarInt::from_u32(4 + reason.len() as u32).encode(&mut payload);
    payload.put_u32(code);
    payload.extend_from_slice(reason.as_bytes());
    payload.freeze()
}

async fn send_data(send: &mut quinn::SendStream, payload: Bytes) {
    let mut wire = BytesMut::new();
    Frame::Data(payload.clone()).encode(&mut wire);
    wire.extend_from_slice(&payload);
    send.write_all(&wire).await.unwrap();
}

fn response_data(mut wire: &[u8]) -> Bytes {
    use h3::proto::varint::VarInt;
    let mut data = BytesMut::new();
    while wire.has_remaining() {
        let kind = VarInt::decode(&mut wire).unwrap().into_inner();
        let length = VarInt::decode(&mut wire).unwrap().into_inner() as usize;
        let value = wire.copy_to_bytes(length);
        if kind == 0 {
            data.extend_from_slice(&value);
        }
    }
    data.freeze()
}

fn assert_reset(error: quinn::ReadToEndError) {
    assert!(
        matches!(error,
            quinn::ReadToEndError::Read(quinn::ReadError::Reset(code))
                if code.into_inner() == SESSION_GONE
        ),
        "unexpected read error: {error:?}"
    );
}

async fn read_var(stream: &mut quinn::RecvStream) -> u64 {
    use h3::proto::varint::VarInt;
    let mut bytes = [0; 8];
    stream.read_exact(&mut bytes[..1]).await.unwrap();
    let size = VarInt::encoded_size(bytes[0]);
    stream.read_exact(&mut bytes[1..size]).await.unwrap();
    VarInt::decode(&mut &bytes[..size]).unwrap().into_inner()
}

#[tokio::test]
async fn peer_close_preserves_metadata_resets_all_directions_and_isolates_sessions() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, mut response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let (_other_connect, _other_response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let sibling = driver.accept_session(req, stream).await.unwrap();

    let (mut bidi_send, mut bidi_recv) = peer.open_bi().await.unwrap();
    let mut wire = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(session.session_id()).encode(&mut wire);
    bidi_send.write_all(&wire).await.unwrap();
    let incoming = match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(_, stream) => stream,
        _ => panic!("expected WT bidi stream"),
    };
    let (mut incoming_send, mut incoming_recv) = h3::quic::BidiStream::split(incoming);
    let mut scratch = [0];
    let reading = incoming_recv.read(&mut scratch);
    tokio::pin!(reading);
    assert!(reading.as_mut().now_or_never().is_none());

    let mut uni_send = peer.open_uni().await.unwrap();
    wire.clear();
    UniStreamHeader::WebTransportUni(session.session_id()).encode(&mut wire);
    uni_send.write_all(&wire).await.unwrap();
    let mut incoming_uni = match driver.accept().await.unwrap().unwrap() {
        IncomingStream::Unidirectional(_, stream) => stream,
        _ => panic!("expected WT uni stream"),
    };
    let mut uni_scratch = [0];
    let uni_reading = incoming_uni.read(&mut uni_scratch);
    tokio::pin!(uni_reading);
    assert!(uni_reading.as_mut().now_or_never().is_none());

    let outgoing = session.open_bi().await.unwrap();
    let (mut outgoing_send, mut outgoing_recv) = h3::quic::BidiStream::split(outgoing);
    let (peer_send, mut peer_recv) = peer.accept_bi().await.unwrap();
    assert_eq!(read_var(&mut peer_recv).await, 0x41);
    let _ = read_var(&mut peer_recv).await;
    let mut outgoing_uni = session.open_uni().await.unwrap();
    let mut retained_control_streams = Vec::new();
    let mut peer_uni = loop {
        let mut recv = peer.accept_uni().await.unwrap();
        if read_var(&mut recv).await == 0x54 {
            let _ = read_var(&mut recv).await;
            break recv;
        }
        retained_control_streams.push(recv);
    };

    let capsule = close_capsule(0xfedcba98, "closing: görüşürüz 🎭");
    // One capsule spans DATA frames and every byte boundary, with cancelled observers.
    for (index, byte) in capsule.iter().enumerate() {
        send_data(&mut connect, Bytes::copy_from_slice(&[*byte])).await;
        let observed = session.closed().now_or_never();
        if index + 1 != capsule.len() {
            assert!(observed.is_none());
        }
    }
    let (first, second) = tokio::join!(session.closed(), session.closed());
    let close = first.unwrap();
    assert_eq!(close.code, 0xfedcba98);
    assert_eq!(close.reason, "closing: görüşürüz 🎭");
    assert_eq!(second.unwrap(), close);
    assert_eq!(session.closed().await.unwrap(), close);
    assert!(reading.await.is_err());
    assert!(uni_reading.await.is_err());
    assert!(incoming_send.write_all(b"closed").await.is_err());
    assert!(outgoing_send.write_all(b"closed").await.is_err());
    assert!(outgoing_recv.read(&mut [0]).await.is_err());
    assert!(outgoing_uni.write_all(b"closed").await.is_err());
    assert_reset(bidi_recv.read_to_end(1024).await.unwrap_err());
    assert_reset(peer_recv.read_to_end(1024).await.unwrap_err());
    assert_reset(peer_uni.read_to_end(1024).await.unwrap_err());
    assert_eq!(
        bidi_send.stopped().await.unwrap().unwrap().into_inner(),
        SESSION_GONE
    );
    assert_eq!(
        uni_send.stopped().await.unwrap().unwrap().into_inner(),
        SESSION_GONE
    );
    assert_eq!(
        peer_send.stopped().await.unwrap().unwrap().into_inner(),
        SESSION_GONE
    );
    assert!(session.open_bi().await.is_err());
    assert!(session.open_uni().await.is_err());
    assert!(session.send_datagram(Bytes::new()).is_err());
    // The close capsule is sufficient: the peer has not sent FIN yet.
    assert!(!response.read_to_end(1024).await.unwrap().is_empty());
    connect.finish().unwrap();

    // Late streams for the ended session are reset as well.
    let (mut late_send, mut late_recv) = peer.open_bi().await.unwrap();
    wire.clear();
    BidiStreamHeader::WebTransportBidi(session.session_id()).encode(&mut wire);
    late_send.write_all(&wire).await.unwrap();
    match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(_, mut stream) => assert!(stream.write_all(b"late").await.is_err()),
        _ => panic!("expected WT stream"),
    }
    assert_reset(late_recv.read_to_end(1024).await.unwrap_err());

    assert!(sibling.closed().now_or_never().is_none());
    sibling.send_datagram(Bytes::from_static(b"alive")).unwrap();
    assert_eq!(
        Datagram::decode(peer.read_datagram().await.unwrap())
            .unwrap()
            .payload(),
        &Bytes::from_static(b"alive")
    );
    let mut sibling_stream = sibling.open_bi().await.unwrap();
    let (_send, mut recv) = peer.accept_bi().await.unwrap();
    let _ = read_var(&mut recv).await;
    let _ = read_var(&mut recv).await;
    sibling_stream.write_all(b"alive").await.unwrap();
    sibling_stream.shutdown().await.unwrap();
    assert_eq!(recv.read_to_end(1024).await.unwrap(), b"alive");
    let (_get, mut recv) = request(&peer, false).await;
    let (_, mut stream) = resolved_request(&mut driver).await;
    stream.send_response(Response::new(())).await.unwrap();
    stream.finish().await.unwrap();
    assert!(!recv.read_to_end(1024).await.unwrap().is_empty());
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn cancelled_local_close_is_resumed_once_by_the_driver() {
    let (_server, _client, peer, mut driver, _control) = pair_with_limits(Some(64), None).await;
    let (mut connect, mut response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let reason = "é".repeat(512);
    assert!(session.close(u32::MAX, &reason).now_or_never().is_none());
    assert_eq!(session.closed().await.unwrap().code, u32::MAX);
    let read = response.read_to_end(4096);
    let drive = driver.accept();
    tokio::pin!(read, drive);
    let received = match futures_util::future::select(read, drive).await {
        futures_util::future::Either::Left((result, _)) => result.unwrap(),
        futures_util::future::Either::Right(_) => panic!("driver stopped before close delivery"),
    };
    assert_eq!(response_data(&received), close_capsule(u32::MAX, &reason));
    session.close(7, "replacement").await.unwrap();
    assert_eq!(session.closed().await.unwrap().reason, reason);
    connect.finish().unwrap();
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn oversized_local_reason_does_not_close_the_session() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, mut response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    assert!(session.close(5, &"x".repeat(1025)).await.is_err());
    assert!(session.closed().now_or_never().is_none());
    session.send_datagram(Bytes::new()).unwrap();
    session.close(42, "server shutdown").await.unwrap();
    assert_eq!(
        response_data(&response.read_to_end(4096).await.unwrap()),
        close_capsule(42, "server shutdown")
    );
    assert_eq!(session.closed().await.unwrap().code, 42);
    connect.finish().unwrap();
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn malformed_capsules_reset_only_the_connect_stream() {
    let malformed = [
        Bytes::from_static(b"\x68\x43\x03\0\0\0"), // Less than the four-byte code.
        Bytes::from_static(b"\x68\x43\x05\0\0\0\0\xff"), // Invalid UTF-8.
        Bytes::from_static(b"\x68\x43\x04\0\0\0\0\x00"), // Data after close.
        Bytes::from_static(b"\x68"),               // Truncated type at FIN.
        Bytes::from_static(b"\x17\x08\x00"),       // Truncated unknown payload.
    ];
    for payload in malformed {
        let (_server, _client, peer, mut driver, _control) = pair().await;
        let (mut connect, mut response) = request(&peer, true).await;
        let (req, stream) = resolved_request(&mut driver).await;
        let session = driver.accept_session(req, stream).await.unwrap();
        send_data(&mut connect, payload).await;
        connect.finish().unwrap();
        assert!(session.closed().await.is_err());
        assert!(matches!(response.read_to_end(4096).await,
            Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(code)))
            if code.into_inner() == h3::error::Code::H3_MESSAGE_ERROR.value()
        ));
        let (_get, mut recv) = request(&peer, false).await;
        let (_, mut stream) = resolved_request(&mut driver).await;
        stream.send_response(Response::new(())).await.unwrap();
        stream.finish().await.unwrap();
        assert!(!recv.read_to_end(1024).await.unwrap().is_empty());
        peer.close(0u32.into(), b"done");
    }
}

#[tokio::test]
async fn local_close_resets_streams_and_survives_dropping_the_session() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, mut response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let (mut send, mut recv) = peer.open_bi().await.unwrap();
    let mut header = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(session.session_id()).encode(&mut header);
    send.write_all(&header).await.unwrap();
    let mut admitted = match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(_, stream) => stream,
        _ => panic!("expected WT stream"),
    };
    {
        let waiting = session.closed();
        tokio::pin!(waiting);
        assert!(waiting.as_mut().now_or_never().is_none());
        session.close(123, "graceful shutdown").await.unwrap();
        assert_eq!(waiting.await.unwrap().code, 123);
    }
    assert!(admitted.write_all(b"late").await.is_err());
    assert_reset(recv.read_to_end(1024).await.unwrap_err());
    assert_eq!(
        send.stopped().await.unwrap().unwrap().into_inner(),
        SESSION_GONE
    );
    // Dropping the session must not reset an already-finished CONNECT send
    // half and erase the close metadata before the peer reads it.
    drop(session);
    assert_eq!(
        response_data(&response.read_to_end(4096).await.unwrap()),
        close_capsule(123, "graceful shutdown")
    );
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn data_after_a_delivered_close_capsule_is_rejected_by_the_driver() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    send_data(&mut connect, close_capsule(4, "done")).await;
    assert_eq!(session.closed().await.unwrap().code, 4);
    send_data(&mut connect, Bytes::from_static(b"not allowed")).await;
    let reading = connect.stopped();
    let drive = driver.accept();
    tokio::pin!(reading, drive);
    match futures_util::future::select(reading, drive).await {
        futures_util::future::Either::Left((result, _)) => {
            assert!(matches!(result,
                Ok(Some(code)) if code.into_inner() == h3::error::Code::H3_MESSAGE_ERROR.value()
            ));
        }
        futures_util::future::Either::Right(_) => panic!("connection closed with its session"),
    }
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn session_close_wakes_an_opener_waiting_for_stream_credit() {
    let (_server, _client, peer, mut driver, _control) = pair_with_limits(None, Some(0)).await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let opening = session.open_bi();
    tokio::pin!(opening);
    assert!(opening.as_mut().now_or_never().is_none());
    session.close(0, "done").await.unwrap();
    assert!(opening.await.is_err());
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn streams_received_before_acceptance_join_the_same_reset_registry() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (mut connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let id = h3_webtransport::SessionId::from(stream.send_id());
    let (mut send, mut recv) = peer.open_bi().await.unwrap();
    let mut wire = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(id).encode(&mut wire);
    send.write_all(&wire).await.unwrap();
    let mut early = match resolver(&mut driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(_, stream) => stream,
        _ => panic!("expected early WT stream"),
    };
    let session = driver.accept_session(req, stream).await.unwrap();
    send_data(&mut connect, close_capsule(5, "done")).await;
    assert_eq!(session.closed().await.unwrap().code, 5);
    assert!(early.write_all(b"late").await.is_err());
    assert_reset(recv.read_to_end(1024).await.unwrap_err());
    peer.close(0u32.into(), b"done");
}

// Expected wire values are independent of the conversion helpers under test.
const APPLICATION_CODES: [(u32, u64); 4] = [
    (0, 0x52e4a40fa8db),
    (29, 0x52e4a40fa8f8),
    (30, 0x52e4a40fa8fa),
    (u32::MAX, 0x52e5ac983162),
];

type Bidi = h3_webtransport::stream::BidiStream<h3_quinn::BidiStream<Bytes>, Bytes>;

async fn data_bidi(
    peer: &quinn::Connection,
    driver: &mut Driver,
    id: h3_webtransport::SessionId,
) -> (quinn::SendStream, quinn::RecvStream, Bidi) {
    let (mut send, recv) = peer.open_bi().await.unwrap();
    let mut wire = BytesMut::new();
    BidiStreamHeader::WebTransportBidi(id).encode(&mut wire);
    send.write_all(&wire).await.unwrap();
    let incoming = match resolver(driver).await.resolve().await.unwrap() {
        AcceptedBi::BidiStream(actual, stream) => {
            assert_eq!(actual, id);
            stream
        }
        _ => panic!("expected WT data stream"),
    };
    (send, recv, incoming)
}

fn assert_application_error(error: std::io::Error, code: Option<u32>, wire: u64) {
    let inner = error
        .get_ref()
        .unwrap()
        .downcast_ref::<h3_webtransport::error::StreamError>()
        .expect("WebTransport I/O error retains its typed cause");
    assert_eq!(inner.application_code(), code);
    assert_eq!(inner.http3_code(), Some(wire));
}

#[tokio::test]
async fn data_stream_reset_and_stop_encode_application_codes_only_once() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    for (code, wire) in APPLICATION_CODES {
        let (mut send, mut recv, mut stream) =
            data_bidi(&peer, &mut driver, session.session_id()).await;
        stream.reset(code);
        assert!(matches!(recv.read_to_end(1024).await,
            Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(actual)))
                if actual.into_inner() == wire
        ));
        // Resetting our send direction leaves the receive direction usable.
        send.write_all(b"still receiving").await.unwrap();
        send.finish().unwrap();
        let mut body = Vec::new();
        stream.read_to_end(&mut body).await.unwrap();
        assert_eq!(body, b"still receiving");

        let (send, mut recv, stream) = data_bidi(&peer, &mut driver, session.session_id()).await;
        let (mut tx, mut rx) = h3::quic::BidiStream::split(stream);
        rx.stop_sending(code);
        assert_eq!(send.stopped().await.unwrap().unwrap().into_inner(), wire);
        // STOP_SENDING does not reset the other direction of this stream.
        tx.write_all(b"still sending").await.unwrap();
        tx.shutdown().await.unwrap();
        assert_eq!(recv.read_to_end(1024).await.unwrap(), b"still sending");
    }
    assert!(session.closed().now_or_never().is_none());
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn data_stream_received_resets_decode_application_codes_for_both_io_traits() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let cases = APPLICATION_CODES
        .into_iter()
        .map(|(app, wire)| (Some(app), wire))
        .chain([
            (None, 0x52e4a40fa8f9), // Reserved HTTP/3 codepoint inside the range.
            (None, SESSION_GONE),
            (None, 0x100),
            (None, 0x52e5ac983163), // One past the last application code.
        ]);
    for (application, wire) in cases {
        let (mut send, _recv, mut stream) =
            data_bidi(&peer, &mut driver, session.session_id()).await;
        send.reset(wire.try_into().unwrap()).unwrap();
        let error = stream.read(&mut [0]).await.unwrap_err();
        assert_application_error(error, application, wire);

        let (mut send, _recv, stream) = data_bidi(&peer, &mut driver, session.session_id()).await;
        let (_tx, mut rx) = h3::quic::BidiStream::split(stream);
        send.reset(wire.try_into().unwrap()).unwrap();
        let error = futures_util::io::AsyncReadExt::read(&mut rx, &mut [0])
            .await
            .unwrap_err();
        assert_application_error(error, application, wire);
    }
    assert!(session.closed().now_or_never().is_none());
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn data_stream_received_stops_decode_application_codes_while_writes_are_pending() {
    let (_server, _client, peer, mut driver, _control) = pair_with_limits(Some(64), None).await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let cases = [
        (Some(0), 0x52e4a40fa8db),
        (Some(30), 0x52e4a40fa8fa),
        (Some(u32::MAX), 0x52e5ac983162),
        (None, SESSION_GONE),
        (None, 0x52e4a40fa8f9),
    ];
    for (application, wire) in cases {
        let (_send, mut recv, mut stream) =
            data_bidi(&peer, &mut driver, session.session_id()).await;
        let writing = stream.write_all(&[0; 1024]);
        tokio::pin!(writing);
        assert!(writing.as_mut().now_or_never().is_none());
        recv.stop(wire.try_into().unwrap()).unwrap();
        assert_application_error(writing.await.unwrap_err(), application, wire);

        let (_send, mut recv, stream) = data_bidi(&peer, &mut driver, session.session_id()).await;
        let (mut tx, _rx) = h3::quic::BidiStream::split(stream);
        let writing = futures_util::io::AsyncWriteExt::write_all(&mut tx, &[0; 1024]);
        tokio::pin!(writing);
        assert!(writing.as_mut().now_or_never().is_none());
        recv.stop(wire.try_into().unwrap()).unwrap();
        assert_application_error(writing.await.unwrap_err(), application, wire);
    }
    assert!(session.closed().now_or_never().is_none());
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn data_stream_unidirectional_handles_map_errors_in_both_directions() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let mut retained_control_streams = Vec::new();
    for (code, wire) in APPLICATION_CODES {
        let mut outgoing = session.open_uni().await.unwrap();
        let mut recv = loop {
            let mut recv = peer.accept_uni().await.unwrap();
            if read_var(&mut recv).await == 0x54 {
                let _ = read_var(&mut recv).await;
                break recv;
            }
            retained_control_streams.push(recv);
        };
        outgoing.reset(code);
        assert!(matches!(recv.read_to_end(1024).await,
            Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(actual)))
                if actual.into_inner() == wire
        ));

        let mut send = peer.open_uni().await.unwrap();
        let mut header = BytesMut::new();
        UniStreamHeader::WebTransportUni(session.session_id()).encode(&mut header);
        send.write_all(&header).await.unwrap();
        let mut incoming = match driver.accept().await.unwrap().unwrap() {
            IncomingStream::Unidirectional(_, stream) => stream,
            _ => panic!("expected WT uni stream"),
        };
        incoming.stop_sending(code);
        assert_eq!(send.stopped().await.unwrap().unwrap().into_inner(), wire);

        let mut send = peer.open_uni().await.unwrap();
        send.write_all(&header).await.unwrap();
        let mut incoming = match driver.accept().await.unwrap().unwrap() {
            IncomingStream::Unidirectional(_, stream) => stream,
            _ => panic!("expected WT uni stream"),
        };
        send.reset(wire.try_into().unwrap()).unwrap();
        let error = incoming.read(&mut [0]).await.unwrap_err();
        assert_application_error(error, Some(code), wire);
    }
    peer.close(0u32.into(), b"done");
}

#[tokio::test]
async fn data_stream_quic_traits_retain_raw_wire_codes() {
    let (_server, _client, peer, mut driver, _control) = pair().await;
    let (_connect, _response) = request(&peer, true).await;
    let (req, stream) = resolved_request(&mut driver).await;
    let session = driver.accept_session(req, stream).await.unwrap();
    let (mut send, mut recv, mut stream) =
        data_bidi(&peer, &mut driver, session.session_id()).await;
    let raw = 0x52e4a40fa8fa;
    h3::quic::SendStream::reset(&mut stream, raw);
    assert!(matches!(recv.read_to_end(1024).await,
        Err(quinn::ReadToEndError::Read(quinn::ReadError::Reset(actual))) if actual.into_inner() == raw
    ));
    send.reset(raw.try_into().unwrap()).unwrap();
    let error =
        futures_util::future::poll_fn(|cx| h3::quic::RecvStream::poll_data(&mut stream, cx))
            .await
            .unwrap_err();
    assert!(
        matches!(error, h3::quic::StreamErrorIncoming::StreamTerminated { error_code } if error_code == raw)
    );
    assert_eq!(
        h3_webtransport::error::StreamError::from(error).application_code(),
        Some(30)
    );
    peer.close(0u32.into(), b"done");
}
