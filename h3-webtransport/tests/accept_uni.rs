use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use h3::server::Connection as H3Connection;
use h3_quinn::quinn;

use h3_webtransport::server::WebTransportSession;

const WT_UNI_STREAM_TYPE: [u8; 2] = [0x40, 0x54];

/// Control stream type, followed by a SETTINGS frame with `ENABLE_CONNECT_PROTOCOL`,
/// `H3_DATAGRAM`, `ENABLE_WEBTRANSPORT` and `WEBTRANSPORT_MAX_SESSIONS`.
const CLIENT_CONTROL_STREAM: [u8; 17] = [
    0x00, 0x04, 0x0e, 0x08, 0x01, 0x33, 0x01, 0xab, 0x60, 0x37, 0x42, 0x01, 0xab, 0x60, 0x37,
    0x43, 0x01,
];

/// HEADERS frame with a QPACK block for `CONNECT https://localhost/` with
/// `:protocol: webtransport`.
fn connect_request() -> Vec<u8> {
    let mut block = vec![0x00, 0x00, 0xcf, 0xd7, 0xc1, 0x50, 0x09];
    block.extend_from_slice(b"localhost");
    block.extend_from_slice(&[0x27, 0x01]);
    block.extend_from_slice(b"protocol");
    block.push(0x0c);
    block.extend_from_slice(b"webtransport");

    let mut frame = vec![0x01, block.len() as u8];
    frame.extend_from_slice(&block);
    frame
}

fn endpoints() -> (quinn::Endpoint, quinn::Endpoint) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key.into())
        .unwrap();
    server_crypto.alpn_protocols = vec![b"h3".to_vec()];
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap(),
    ));
    let server = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert_der).unwrap();
    let mut client_crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![b"h3".to_vec()];
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto).unwrap(),
    )));

    (server, client)
}

/// `accept_bi` and `accept_uni` are polled by different tasks, and the header of the
/// unidirectional stream is resolved by whichever polls the connection first.
#[tokio::test]
async fn accept_uni_is_woken_by_accept_bi() {
    let (server, client) = endpoints();
    let addr = server.local_addr().unwrap();

    let client_task = tokio::spawn(async move {
        let conn = client.connect(addr, "localhost").unwrap().await.unwrap();

        // `h3::client` cannot enable WebTransport, so the client speaks raw HTTP/3.
        let mut control = conn.open_uni().await.unwrap();
        control.write_all(&CLIENT_CONTROL_STREAM).await.unwrap();

        let (mut connect_send, mut connect_recv) = conn.open_bi().await.unwrap();
        connect_send.write_all(&connect_request()).await.unwrap();
        // Wait for the response (HEADERS frame) to the CONNECT request.
        let mut response = [0; 1];
        connect_recv.read_exact(&mut response).await.unwrap();
        assert_eq!(response, [0x01]);

        // Let the server start accepting, so that the stream arrives while it is pending.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // The CONNECT stream is the first client initiated bidirectional stream.
        let mut uni = conn.open_uni().await.unwrap();
        uni.write_all(&WT_UNI_STREAM_TYPE).await.unwrap();
        uni.write_all(&[0]).await.unwrap();
        uni.write_all(b"hello").await.unwrap();
        uni.finish().unwrap();

        // Keep the connection open until the server is done.
        conn.closed().await;
    });

    let incoming = server.accept().await.unwrap();
    let conn = incoming.await.unwrap();
    let mut h3_conn: H3Connection<_, Bytes> = h3::server::builder()
        .enable_webtransport(true)
        .enable_extended_connect(true)
        .enable_datagram(true)
        .max_webtransport_sessions(1)
        .build(h3_quinn::Connection::new(conn))
        .await
        .unwrap();

    let resolver = h3_conn.accept().await.unwrap().unwrap();
    let (request, stream) = resolver.resolve_request().await.unwrap();
    let session = WebTransportSession::accept(request, stream, h3_conn)
        .await
        .unwrap();

    // Separate tasks, as in a server handling streams concurrently. `accept_uni` is polled
    // first, so `accept_bi` replaces the waker of the stream.
    let session = Arc::new(session);
    let accept_uni = tokio::spawn({
        let session = session.clone();
        async move { session.accept_uni().await.unwrap().unwrap().0 }
    });
    // The client never opens a bidirectional stream, so this stays pending.
    let accept_bi = tokio::spawn({
        let session = session.clone();
        async move { session.accept_bi().await }
    });
    let accepted = tokio::time::timeout(Duration::from_secs(5), accept_uni).await;

    accept_bi.abort();
    client_task.abort();
    let session_id = accepted.expect("accept_uni was not woken up").unwrap();
    assert_eq!(session_id, session.session_id());
}
