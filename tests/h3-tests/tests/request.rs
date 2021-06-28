use std::{error::Error as _, time::Duration};

use assert_matches::assert_matches;
use http::{HeaderMap, Request, Response, StatusCode};

use h3::{
    client,
    error::{Code, Kind},
    server, ConnectionState,
};
use h3_tests::Pair;

#[tokio::test]
async fn get() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_, mut client) = client::new(pair.client().await).await.expect("client init");
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
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_, mut client) = client::new(pair.client().await).await.expect("client init");
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
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_, mut client) = client::new(pair.client().await).await.expect("client init");
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
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_, mut client) = client::new(pair.client().await).await.expect("client init");
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

#[tokio::test]
async fn header_too_big_response_from_server() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        // Do not poll driver so client doesn't know about server's max_field section size setting
        let (_conn, mut client) = client::new(pair.client().await).await.expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.finish().await.expect("client finish");
        let response = request_stream.recv_response().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::builder()
            .max_field_section_size(12)
            .build(conn)
            .await
            .unwrap();

        let err_kind = incoming_req.accept().await.map(|_| ()).unwrap_err().kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 179,
                max_size: 12,
            }
        );
        let _ = incoming_req.accept().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_response_from_server_trailers() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        // Do not poll driver so client doesn't know about server's max_field_section_size setting
        let (_conn, mut client) = client::new(pair.client().await).await.expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream
            .send_data("wonderful json".into())
            .await
            .expect("send_data");

        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "A".repeat(200).parse().unwrap());
        request_stream
            .send_trailers(trailers)
            .await
            .expect("send trailers");
        request_stream.finish().await.expect("client finish");

        let response = request_stream.recv_response().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
        );
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::builder()
            .max_field_section_size(207)
            .build(conn)
            .await
            .unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        let _ = request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("body");
        let err_kind = request_stream.recv_trailers().await.unwrap_err().kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 239,
                max_size: 207,
            }
        );
        let _ = incoming_req.accept().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_client_error() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_, mut client) = client::new(pair.client().await).await.expect("client init");
        // pretend client already received server's settings
        client
            .shared_state()
            .write("client")
            .peer_max_field_section_size = 12;

        let req = Request::get("http://localhost/salut").body(()).unwrap();
        let err_kind = client
            .send_request(req)
            .await
            .map(|_| ())
            .unwrap_err()
            .kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 179,
                max_size: 12,
            }
        );
    };

    let server_fut = async {
        server.next().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_client_error_trailer() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        // Do not poll driver so client doesn't know about server's max_field_section_size setting
        let (_conn, mut client) = client::new(pair.client().await).await.expect("client init");
        client
            .shared_state()
            .write("client")
            .peer_max_field_section_size = 200;

        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream
            .send_data("wonderful json".into())
            .await
            .expect("send_data");

        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "A".repeat(200).parse().unwrap());

        let err_kind = request_stream
            .send_trailers(trailers)
            .await
            .unwrap_err()
            .kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 239,
                max_size: 200,
            }
        );

        request_stream.finish().await.expect("client finish");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::builder()
            .max_field_section_size(207)
            .build(conn)
            .await
            .unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        let _ = request_stream
            .recv_data()
            .await
            .expect("recv data")
            .expect("body");
        let _ = incoming_req.accept().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_discard_from_client() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        // Do not poll driver so client doesn't know about server's max_field section size setting
        let (_conn, mut client) = client::builder()
            .max_field_section_size(12)
            .build(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.finish().await.expect("client finish");
        let err_kind = request_stream.recv_response().await.unwrap_err().kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 42,
                max_size: 12,
            }
        );

        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.finish().await.expect("client finish");
        let _ = request_stream.recv_response().await.unwrap_err();
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        // pretend server didn't receive settings
        incoming_req
            .shared_state()
            .write("client")
            .peer_max_field_section_size = u64::MAX;
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .expect("send_response");

        // Keep sending: wait for the stream to be cancelled by the client
        let mut err = None;
        for _ in 0..100 {
            if let Err(e) = request_stream.send_data("some data".into()).await {
                err = Some(e);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_matches!(err.as_ref().unwrap().kind(), Kind::Transport);
        assert_matches!(err, Some(e) => {
            assert_matches!(
                e.source()
                    .unwrap()
                    .downcast_ref::<h3_quinn::SendStreamError>()
                    .unwrap(),
                h3_quinn::SendStreamError::Write(h3_quinn::quinn::WriteError::Stopped(c))
                    if Code::H3_REQUEST_REJECTED == c.into_inner()
            );
        });
        let _ = incoming_req.accept().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_discard_from_client_trailers() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        // Do not poll driver so client doesn't know about server's max_field section size setting
        let (_conn, mut client) = client::builder()
            .max_field_section_size(200)
            .build(pair.client().await)
            .await
            .expect("client init");
        let mut request_stream = client
            .send_request(Request::get("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        request_stream.recv_response().await.expect("recv response");
        request_stream.recv_data().await.expect("recv data");

        let err_kind = request_stream.recv_trailers().await.unwrap_err().kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 539,
                max_size: 200,
            }
        );
        request_stream.finish().await.expect("client finish");
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();

        // pretend server didn't receive settings
        incoming_req
            .shared_state()
            .write("server")
            .peer_max_field_section_size = u64::MAX;

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
        trailers.insert("trailer", "value".repeat(100).parse().unwrap());
        request_stream
            .send_trailers(trailers)
            .await
            .expect("send_trailers");
        request_stream.finish().await.expect("finish");

        let _ = incoming_req.accept().await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_server_error() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_conn, mut client) = client::new(pair.client().await) // header size limit faked for brevity
            .await
            .expect("client init");

        let req = Request::get("http://localhost/salut").body(()).unwrap();
        let _ = client
            .send_request(req)
            .await
            .unwrap()
            .recv_response()
            .await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();
        // pretend the server already received client's settings
        incoming_req
            .shared_state()
            .write("server")
            .peer_max_field_section_size = 12;

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        let err_kind = request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .map(|_| ())
            .unwrap_err()
            .kind();

        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 42,
                max_size: 12
            }
        );
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn header_too_big_server_error_trailers() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (_conn, mut client) = client::new(pair.client().await) // header size limit faked for brevity
            .await
            .expect("client init");

        let req = Request::get("http://localhost/salut").body(()).unwrap();
        let _ = client
            .send_request(req)
            .await
            .unwrap()
            .recv_response()
            .await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();
        // pretend the server already received client's settings
        incoming_req
            .shared_state()
            .write("write")
            .peer_max_field_section_size = 200;

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();
        request_stream
            .send_response(
                Response::builder()
                    .status(200)
                    .body(())
                    .expect("build response"),
            )
            .await
            .unwrap();
        request_stream
            .send_data("wonderful hypertext".into())
            .await
            .expect("send_data");

        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".repeat(100).parse().unwrap());
        let err_kind = request_stream
            .send_trailers(trailers)
            .await
            .unwrap_err()
            .kind();
        assert_matches!(
            err_kind,
            Kind::HeaderTooBig {
                actual_size: 539,
                max_size: 200,
            }
        );
    };

    tokio::join!(server_fut, client_fut);
}
