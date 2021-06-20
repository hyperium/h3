use futures::future;
use h3::{client, server};
use h3_tests::Pair;
use std::time::Duration;

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
                if client.state().0.read().unwrap().peer_max_field_section_size == 12 {
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

        let state = incoming.state().clone();
        let accept = async { incoming.accept().await.unwrap() };

        let settings_change = async {
            for _ in 0..10 {
                if state.0.read().unwrap().peer_max_field_section_size == 12 {
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
