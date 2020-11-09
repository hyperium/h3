use h3::{client, server};
use http::{HeaderMap, Request, Response, StatusCode};

use h3_tests::Pair;

#[tokio::test]
async fn get() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let mut client = client::Connection::new(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");

        let response = request_stream.recv_response().await.expect("recv response");
        assert_eq!(response.status(), StatusCode::OK);

        let body = request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("body");
        assert_eq!(body.as_ref(), b"wonderful hypertext");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .expect("send_response");
        request_stream
            .send_data("wonderful hypertext".into())
            .await
            .expect("send_data");
        request_stream.finish().await.expect("finish");
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn get_with_trailers_unknown_content_type() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let mut client = client::Connection::new(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.recv_response().await.expect("recv response");
        request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("body");

        assert!(request_stream.recv_data().await.unwrap().is_none());
        let trailers = request_stream
            .recv_trailers()
            .await
            .expect("recv trailers")
            .expect("trailers none");
        assert_eq!(trailers.get("trailer").unwrap(), &"value");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .expect("send_response");
        request_stream
            .send_data("wonderful hypertext".into())
            .await
            .expect("send_data");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        request_stream
            .send_trailers(trailers)
            .await
            .expect("send_trailers");
        request_stream.finish().await.expect("finish");
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn get_with_trailers_known_content_type() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let mut client = client::Connection::new(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.recv_response().await.expect("recv response");
        request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("body");

        let trailers = request_stream
            .recv_trailers()
            .await
            .expect("recv trailers")
            .expect("trailers none");
        assert_eq!(trailers.get("trailer").unwrap(), &"value");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .expect("send_response");
        request_stream
            .send_data("wonderful hypertext".into())
            .await
            .expect("send_data");

        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        request_stream
            .send_trailers(trailers)
            .await
            .expect("send_trailers");
        request_stream.finish().await.expect("finish");
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn post() {
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let mut client = client::Connection::new(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");

        request_stream
            .send_data("wonderful json".into())
            .await
            .expect("send_data");
        request_stream.finish().await.expect("client finish");

        request_stream.recv_response().await.expect("recv response");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .expect("send_response");

        let request_body = request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("server recv body");
        assert_eq!(request_body, "wonderful json");
        request_stream.finish().await.expect("client finish");
    };

    tokio::join!(server_fut, client_fut);
}
