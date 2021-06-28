use std::time::Duration;

use assert_matches::assert_matches;
use futures::{future, StreamExt};
use http::Request;

use h3::{client, error::{Code, Kind}, server, ConnectionState};
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
                Kind::Application { reason: Some(reason), code: Code::H3_STREAM_CREATION_ERROR }
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
                Err(quinn::WriteError::ConnectionClosed(
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
