use h3::{client, server};
use h3_tests::Pair;

#[tokio::test]
async fn connect() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let _client = client::Connection::new(pair.client().await)
            .await
            .expect("client init");
    };

    let server_fut = async {
        let conn = server.next().await;
        let _incoming_req = server::Connection::new(conn).await.unwrap();
    };

    tokio::join!(server_fut, client_fut);
}
