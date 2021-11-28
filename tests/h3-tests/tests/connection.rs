use std::time::Duration;

use assert_matches::assert_matches;
use bytes::BytesMut;
use futures::{future, StreamExt};
use http::Request;

use h3::{
    client,
    error::{Code, Kind},
    server,
    test_helpers::{
        proto::{coding::Encode as _, frame::Frame, stream::StreamType},
        ConnectionState,
    },
};
use h3_quinn::quinn;
use h3_tests::Pair;

#[tokio::test]
async fn connect() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let _ = client::new(pair.client().await).await.expect("client init");
    };

    let server_fut = async {
        let conn = server.next().await;
        let _ = server::Connection::new(conn).await.unwrap();
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn accept_request_end_on_client_close() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let _ = client::new(pair.client().await).await.expect("client init");
        // client is dropped, it will send H3_NO_ERROR
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        // Accept returns Ok(None)
        assert!(incoming.accept().await.unwrap().is_none());
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn server_drop_close() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let server_fut = async {
        let conn = server.next().await;
        let _ = server::Connection::new(conn).await.unwrap();
    };

    let (mut conn, mut send) = client::new(pair.client().await).await.expect("client init");
    let client_fut = async {
        let request_fut = async move {
            let mut request_stream = send
                .send_request(Request::get("http://no.way").body(()).unwrap())
                .await
                .unwrap();
            let response = request_stream.recv_response().await;
            assert_matches!(response.unwrap_err().kind(), Kind::Closed);
        };

        let drive_fut = async {
            let drive = future::poll_fn(|cx| conn.poll_close(cx)).await;
            assert_matches!(drive, Ok(()));
        };
        tokio::join!(request_fut, drive_fut);
    };
    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn client_close_only_on_last_sender_drop() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        assert!(incoming.accept().await.unwrap().is_some());
        assert!(incoming.accept().await.unwrap().is_some());
        assert!(incoming.accept().await.unwrap().is_none());
    };

    let client_fut = async {
        let (mut conn, mut send1) = client::new(pair.client().await).await.expect("client init");
        let mut send2 = send1.clone();
        let _ = send1
            .send_request(Request::get("http://no.way").body(()).unwrap())
            .await
            .unwrap()
            .finish()
            .await;
        let _ = send2
            .send_request(Request::get("http://no.way").body(()).unwrap())
            .await
            .unwrap()
            .finish()
            .await;
        drop(send1);
        drop(send2);

        let drive = future::poll_fn(|cx| conn.poll_close(cx)).await;
        assert_matches!(drive, Ok(()));
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn settings_exchange_client() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (mut conn, client) = client::new(pair.client().await).await.expect("client init");
        let settings_change = async {
            for _ in 0..10 {
                if client
                    .shared_state()
                    .read("client")
                    .peer_max_field_section_size
                    == 12
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            panic!("peer's max_field_section_size didn't change");
        };

        let drive = async move {
            future::poll_fn(|cx| conn.poll_close(cx)).await.unwrap();
        };

        tokio::select! { _ = settings_change => (), _ = drive => panic!("driver resolved first") };
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::builder()
            .max_field_section_size(12)
            .build(conn)
            .await
            .unwrap();
        incoming.accept().await.unwrap()
    };

    tokio::select! { _ = server_fut => panic!("server resolved first"), _ = client_fut => () };
}

#[tokio::test]
async fn settings_exchange_server() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (mut conn, _client) = client::builder()
            .max_field_section_size(12)
            .build(pair.client().await)
            .await
            .expect("client init");
        let drive = async move {
            future::poll_fn(|cx| conn.poll_close(cx)).await.unwrap();
        };

        tokio::select! { _ = drive => () };
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();

        let state = incoming.shared_state().clone();
        let accept = async { incoming.accept().await.unwrap() };

        let settings_change = async {
            for _ in 0..10 {
                if state.read("setting_change").peer_max_field_section_size == 12 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            panic!("peer's max_field_section_size didn't change");
        };
        tokio::select! { _ = accept => panic!("server resolved first"), _ = settings_change => () };
    };

    tokio::select! { _ = server_fut => (), _ = client_fut => () };
}

#[tokio::test]
async fn client_error_on_bidi_recv() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    macro_rules! check_err {
        ($e:expr) => {
            assert_matches!(
                $e.map(|_| ()).unwrap_err().kind(),
                Kind::Application { reason: Some(reason), code: Code::H3_STREAM_CREATION_ERROR, .. }
                if *reason == *"client received a bidirectionnal stream");
        }
    }

    let client_fut = async {
        let (mut conn, mut send) = client::new(pair.client().await).await.expect("client init");
        let driver = future::poll_fn(|cx| conn.poll_close(cx));
        check_err!(driver.await);
        check_err!(
            send.send_request(Request::get("http://no.way").body(()).unwrap())
                .await
        );
    };

    let server_fut = async {
        let quinn::NewConnection { connection, .. } =
            server.incoming.next().await.unwrap().await.unwrap();
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        for _ in 0..100 {
            match send.write(b"I'm not really a server").await {
                Err(quinn::WriteError::ConnectionLost(
                    quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
                        error_code,
                        ..
                    }),
                )) if Code::H3_STREAM_CREATION_ERROR == error_code.into_inner() => break,
                Err(e) => panic!("got err: {}", e),
                Ok(_) => tokio::time::sleep(Duration::from_millis(1)).await,
            }
        }
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn two_control_streams() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let new_connection = pair.client_inner().await;

        for _ in 0..=1 {
            let mut control_stream = new_connection.connection.open_uni().await.unwrap();
            let mut buf = BytesMut::new();
            StreamType::CONTROL.encode(&mut buf);
            control_stream.write_all(&buf[..]).await.unwrap();
        }

        tokio::time::sleep(Duration::from_secs(10)).await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        assert_matches!(
            incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Application {
                code: Code::H3_STREAM_CREATION_ERROR,
                ..
            }
        );
    };

    tokio::select! { _ = server_fut => (), _ = client_fut => panic!("client resolved first") };
}

#[tokio::test]
async fn control_close_send_error() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let new_connection = pair.client_inner().await;
        let mut control_stream = new_connection.connection.open_uni().await.unwrap();

        let mut buf = BytesMut::new();
        StreamType::CONTROL.encode(&mut buf);
        control_stream.write_all(&buf[..]).await.unwrap();
        control_stream.finish().await.unwrap(); // close the client control stream immediately

        let (mut driver, _send) = client::new(h3_quinn::Connection::new(new_connection))
            .await
            .unwrap();

        future::poll_fn(|cx| driver.poll_close(cx)).await
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        // Driver detects that the recieving side of the control stream has been closed
        assert_matches!(
        incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Application { reason: Some(reason), code: Code::H3_CLOSED_CRITICAL_STREAM, .. }
            if *reason == *"control stream closed");
        // Poll it once again returns the previously stored error
        assert_matches!(
            incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Application { reason: Some(reason), code: Code::H3_CLOSED_CRITICAL_STREAM, .. }
            if *reason == *"control stream closed");
    };

    tokio::select! { _ = server_fut => (), _ = client_fut => panic!("client resolved first") };
}

#[tokio::test]
async fn missing_settings() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let new_connection = pair.client_inner().await;
        let mut control_stream = new_connection.connection.open_uni().await.unwrap();

        let mut buf = BytesMut::new();
        StreamType::CONTROL.encode(&mut buf);
        // Send a non-settings frame before the mandatory Settings frame on control stream
        Frame::CancelPush(0).encode(&mut buf);
        control_stream.write_all(&buf[..]).await.unwrap();

        tokio::time::sleep(Duration::from_secs(10)).await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        assert_matches!(
            incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Application {
                code: Code::H3_MISSING_SETTINGS,
                ..
            }
        );
    };

    tokio::select! { _ = server_fut => (), _ = client_fut => panic!("client resolved first") };
}

#[tokio::test]
async fn control_stream_frame_unexpected() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let new_connection = pair.client_inner().await;
        let mut control_stream = new_connection.connection.open_uni().await.unwrap();

        let mut buf = BytesMut::new();
        StreamType::CONTROL.encode(&mut buf);
        Frame::Data { len: 0 }.encode(&mut buf);
        control_stream.write_all(&buf[..]).await.unwrap();

        tokio::time::sleep(Duration::from_secs(10)).await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        assert_matches!(
            incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Application {
                code: Code::H3_FRAME_UNEXPECTED,
                ..
            }
        );
    };

    tokio::select! { _ = server_fut => (), _ = client_fut => panic!("client resolved first") };
}

#[tokio::test]
async fn timeout_on_control_frame_read() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    pair.with_timeout(Duration::from_millis(10));

    let mut server = pair.server();

    let client_fut = async {
        let (mut driver, _send_request) = client::new(pair.client().await).await.unwrap();
        let _ = future::poll_fn(|cx| driver.poll_close(cx)).await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        assert_matches!(
            incoming.accept().await.map(|_| ()).unwrap_err().kind(),
            Kind::Timeout
        );
    };

    tokio::join!(server_fut, client_fut);
}
