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
    client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
    )));
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
