use std::time::Duration;

use assert_matches::assert_matches;
use bytes::{BufMut, BytesMut};
use futures::future;
use h3_quinn::ReadError;
use http::{request, HeaderMap, Request, Response, StatusCode};

use h3::{
    client,
    error::{Code, Kind},
    server,
    test_helpers::{
        proto::{
            coding::Encode,
            frame::{self, Frame},
            headers::Header,
        },
        qpack, ConnectionState,
    },
};

use h3_tests::Pair;

#[tokio::test]
async fn get() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
                ..
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
                ..
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
                    ..
                }
            );
        };
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await).await.expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
                    ..
                }
            );

            request_stream.finish().await.expect("client finish");
        };
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
                ..
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
        assert_matches!(
            err.as_ref().unwrap().kind(),
            Kind::Application {
                code: Code::H3_REQUEST_CANCELLED,
                ..
            }
        );
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
        let (mut driver, mut client) = client::builder()
            .max_field_section_size(200)
            .build(pair.client().await)
            .await
            .expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
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
                    ..
                }
            );
            request_stream.finish().await.expect("client finish");
        };
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
        let (mut driver, mut client) = client::new(pair.client().await) // header size limit faked for brevity
            .await
            .expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
            let req = Request::get("http://localhost/salut").body(()).unwrap();
            let _ = client
                .send_request(req)
                .await
                .unwrap()
                .recv_response()
                .await;
        };
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_request, mut request_stream) = incoming_req.accept().await.expect("accept").unwrap();

        // pretend the server received a smaller max_field_section_size
        incoming_req
            .shared_state()
            .write("server")
            .peer_max_field_section_size = 12;

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
                max_size: 12,
                ..
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
        let (mut driver, mut client) = client::new(pair.client().await) // header size limit faked for brevity
            .await
            .expect("client init");
        let drive_fut = async { future::poll_fn(|cx| driver.poll_close(cx)).await };
        let req_fut = async {
            let req = Request::get("http://localhost/salut").body(()).unwrap();
            let _ = client
                .send_request(req)
                .await
                .unwrap()
                .recv_response()
                .await;
        };
        tokio::select! { _ = req_fut => (), _ = drive_fut => () }
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
            .unwrap();
        request_stream
            .send_data("wonderful hypertext".into())
            .await
            .expect("send_data");

        // pretend the server already received client's settings
        incoming_req
            .shared_state()
            .write("write")
            .peer_max_field_section_size = 200;

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
                ..
            }
        );
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn get_timeout_client_recv_response() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    pair.with_timeout(Duration::from_millis(100));
    let mut server = pair.server();

    let client_fut = async {
        let (mut conn, mut client) = client::new(pair.client().await).await.expect("client init");
        let request_fut = async {
            let mut request_stream = client
                .send_request(Request::get("http://localhost/salut").body(()).unwrap())
                .await
                .expect("request");

            let response = request_stream.recv_response().await;
            assert_matches!(response.unwrap_err().kind(), h3::error::Kind::Timeout);
        };

        let drive_fut = async move {
            let result = future::poll_fn(|cx| conn.poll_close(cx)).await;
            assert_matches!(result.unwrap_err().kind(), h3::error::Kind::Timeout);
        };

        tokio::join!(drive_fut, request_fut);
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        // _req must not be dropped, else the connection will be closed and the timeout
        // wont be triggered
        let _req = incoming_req.accept().await.expect("accept").unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn get_timeout_client_recv_data() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    pair.with_timeout(Duration::from_millis(200));
    let mut server = pair.server();

    let client_fut = async {
        let (mut conn, mut client) = client::new(pair.client().await).await.expect("client init");
        let request_fut = async {
            let mut request_stream = client
                .send_request(Request::get("http://localhost/salut").body(()).unwrap())
                .await
                .expect("request");

            let _ = request_stream.recv_response().await.unwrap();
            let data = request_stream.recv_data().await;
            assert_matches!(data.unwrap_err().kind(), h3::error::Kind::Timeout);
        };

        let drive_fut = async move {
            let result = future::poll_fn(|cx| conn.poll_close(cx)).await;
            assert_matches!(result.unwrap_err().kind(), h3::error::Kind::Timeout);
        };

        tokio::join!(drive_fut, request_fut);
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
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn get_timeout_server_accept() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    pair.with_timeout(Duration::from_millis(200));
    let mut server = pair.server();

    let client_fut = async {
        let (mut conn, _client) = client::new(pair.client().await).await.expect("client init");
        let request_fut = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
        };

        let drive_fut = async move {
            let result = future::poll_fn(|cx| conn.poll_close(cx)).await;
            assert_matches!(result.unwrap_err().kind(), h3::error::Kind::Timeout);
        };

        tokio::join!(drive_fut, request_fut);
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        assert_matches!(
            incoming_req.accept().await.map(|_| ()).unwrap_err().kind(),
            h3::error::Kind::Timeout
        );
    };

    tokio::join!(server_fut, client_fut);
}

#[tokio::test]
async fn post_timeout_server_recv_data() {
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    pair.with_timeout(Duration::from_millis(100));
    let mut server = pair.server();

    let client_fut = async {
        let (_conn, mut client) = client::new(pair.client().await).await.expect("client init");
        let _request_stream = client
            .send_request(Request::post("http://localhost/salut").body(()).unwrap())
            .await
            .expect("request");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming_req = server::Connection::new(conn).await.unwrap();

        let (_, mut req_stream) = incoming_req.accept().await.expect("accept").unwrap();
        assert_matches!(
            req_stream.recv_data().await.unwrap_err().kind(),
            h3::error::Kind::Timeout
        );
    };

    tokio::join!(server_fut, client_fut);
}

// 4.1. HTTP Message Exchanges

// An HTTP message (request or response) consists of:
// * the header section, sent as a single HEADERS frame (see Section 7.2.2),
// * optionally, the content, if present, sent as a series of DATA frames (see Section 7.2.1),
// * and optionally, the trailer section, if present, sent as a single HEADERS frame.

#[tokio::test]
async fn request_valid_one_header() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
    })
    .await;
}

#[tokio::test]
async fn request_valid_header_data() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_valid_header_data_trailer() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
    })
    .await;
}

#[tokio::test]
async fn request_valid_header_multiple_data_trailer() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
    })
    .await;
}

#[tokio::test]
async fn request_valid_header_trailer() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
    })
    .await;
}

// Frames of unknown types (Section 9), including reserved frames (Section
// 7.2.8) MAY be sent on a request or push stream before, after, or interleaved
// with other frames described in this section.

#[tokio::test]
async fn request_valid_unkown_frame_before() {
    request_sequence_ok(|mut buf| {
        unknown_frame_encode(buf);
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_after_one_header() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        unknown_frame_encode(buf);
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_interleaved_after_header() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        unknown_frame_encode(buf);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_interleaved_between_data() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        unknown_frame_encode(buf);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_interleaved_after_data() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        unknown_frame_encode(buf);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_interleaved_before_trailers() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        unknown_frame_encode(buf);
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
    })
    .await;
}

#[tokio::test]
async fn request_valid_unkown_frame_after_trailers() {
    request_sequence_ok(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
        unknown_frame_encode(buf);
    })
    .await;
}

// [...]
// Receipt of an invalid sequence of frames MUST be treated as a connection error of type H3_FRAME_UNEXPECTED
fn invalid_request_frames() -> Vec<Frame> {
    vec![
        Frame::CancelPush(0),
        Frame::Settings(frame::Settings::default()),
        Frame::Goaway(1),
        Frame::MaxPushId(1),
        Frame::DuplicatePush(1),
    ]
}

#[tokio::test]
async fn request_invalid_frame_first() {
    for frame in invalid_request_frames() {
        request_sequence_unexpected(|mut buf| frame.encode(&mut buf)).await;
    }
}

#[tokio::test]
async fn request_invalid_frame_after_header() {
    for frame in invalid_request_frames() {
        request_sequence_unexpected(|mut buf| {
            request_encode(
                &mut buf,
                Request::post("http://localhost/salut").body(()).unwrap(),
            );
            frame.encode(&mut buf);
        })
        .await;
    }
}

#[tokio::test]
async fn request_invalid_frame_after_data() {
    for frame in invalid_request_frames() {
        request_sequence_unexpected(|mut buf| {
            request_encode(
                &mut buf,
                Request::post("http://localhost/salut").body(()).unwrap(),
            );
            Frame::Data { len: 4 }.encode(&mut buf);
            buf.put_slice(b"fada");
            frame.encode(&mut buf);
        })
        .await;
    }
}

#[tokio::test]
async fn request_invalid_frame_after_trailers() {
    for frame in invalid_request_frames() {
        request_sequence_unexpected(|mut buf| {
            request_encode(
                &mut buf,
                Request::post("http://localhost/salut").body(()).unwrap(),
            );
            Frame::Data { len: 4 }.encode(&mut buf);
            buf.put_slice(b"fada");
            let mut trailers = HeaderMap::new();
            trailers.insert("trailer", "value".parse().unwrap());
            trailers_encode(buf, trailers);
            frame.encode(&mut buf);
        })
        .await;
    }
}

#[tokio::test]
async fn request_invalid_data_after_trailers() {
    request_sequence_unexpected(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_invalid_data_first() {
    request_sequence_unexpected(|mut buf| {
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}

#[tokio::test]
async fn request_invalid_two_trailers() {
    request_sequence_unexpected(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers.clone());
        trailers_encode(buf, trailers);
    })
    .await;
}

// 7.1. Frame Layout
// [...]
// Each frame's payload MUST contain exactly the fields identified in its
// description. A frame payload that contains additional bytes after the
// identified fields or a frame payload that terminates before the end of the
// identified fields MUST be treated as a connection error of type
// H3_FRAME_ERROR; see Section 8.

#[tokio::test]
async fn request_invalid_trailing_byte() {
    request_sequence_frame_error(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 4 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
        buf.put_u8(255);
    })
    .await;
}

#[tokio::test]
async fn request_invalid_data_frame_length_too_large() {
    request_sequence_frame_error(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 5 }.encode(&mut buf);
        buf.put_slice(b"fada");
        let mut trailers = HeaderMap::new();
        trailers.insert("trailer", "value".parse().unwrap());
        trailers_encode(buf, trailers);
    })
    .await;
}

#[tokio::test]
async fn request_invalid_data_frame_length_too_short() {
    request_sequence_frame_error(|mut buf| {
        request_encode(
            &mut buf,
            Request::post("http://localhost/salut").body(()).unwrap(),
        );
        Frame::Data { len: 3 }.encode(&mut buf);
        buf.put_slice(b"fada");
    })
    .await;
}
// Helpers

fn request_encode<B: BufMut>(buf: &mut B, req: http::Request<()>) {
    let (parts, _) = req.into_parts();
    let request::Parts {
        method,
        uri,
        headers,
        ..
    } = parts;
    let headers = Header::request(method, uri, headers).unwrap();
    let mut block = BytesMut::new();
    qpack::encode_stateless(&mut block, headers).unwrap();
    Frame::Headers(block.freeze()).encode(buf);
}

fn trailers_encode<B: BufMut>(buf: &mut B, fields: HeaderMap) {
    let headers = Header::trailer(fields);
    let mut block = BytesMut::new();
    qpack::encode_stateless(&mut block, headers).unwrap();
    Frame::Headers(block.freeze()).encode(buf);
}

fn unknown_frame_encode<B: BufMut>(buf: &mut B) {
    buf.put_slice(&[22, 4, 0, 255, 128, 0]);
}

async fn request_sequence_ok<F>(request: F)
where
    F: Fn(&mut BytesMut),
{
    request_sequence_check(request, |res| assert!(res.is_ok())).await;
}

async fn request_sequence_unexpected<F>(request: F)
where
    F: Fn(&mut BytesMut),
{
    request_sequence_check(request, |err| {
        assert_matches!(
            err.unwrap_err().kind(),
            Kind::Application {
                code: Code::H3_FRAME_UNEXPECTED,
                ..
            }
        )
    })
    .await;
}

async fn request_sequence_frame_error<F>(request: F)
where
    F: Fn(&mut BytesMut),
{
    request_sequence_check(request, |err| {
        assert_matches!(
            err.unwrap_err().kind(),
            Kind::Application {
                code: Code::H3_FRAME_ERROR,
                ..
            }
        )
    })
    .await;
}

async fn request_sequence_check<F, FC>(request: F, check: FC)
where
    F: Fn(&mut BytesMut),
    FC: Fn(Result<(), h3::Error>),
{
    h3_tests::init_tracing();
    let mut pair = Pair::new();
    let mut server = pair.server();

    let client_fut = async {
        let new_connection = pair.client_inner().await;
        let (mut req_send, mut req_recv) = new_connection.connection.open_bi().await.unwrap();

        let mut buf = BytesMut::new();
        request(&mut buf);
        req_send.write_all(&buf[..]).await.unwrap();
        req_send.finish().await.unwrap();

        let res = req_recv
            .read(&mut buf)
            .await
            .map_err(Into::<ReadError>::into)
            .map_err(Into::<h3::Error>::into)
            .map(|_| ());
        check(res);

        let (mut driver, _send) = client::new(h3_quinn::Connection::new(new_connection))
            .await
            .unwrap();

        let res = future::poll_fn(|cx| driver.poll_close(cx))
            .await
            .map_err(Into::<h3::Error>::into)
            .map(|_| ());
        check(res);
    };

    let server_fut = async {
        let conn = server.next().await;
        let mut incoming = server::Connection::new(conn).await.unwrap();
        let (_, mut stream) = incoming
            .accept()
            .await?
            .expect("request stream end unexpected");
        while stream.recv_data().await?.is_some() {}
        stream.recv_trailers().await?;
        Result::<(), h3::Error>::Ok(())
    };

    tokio::select! { res = server_fut => check(res)
    , _ = client_fut => panic!("client resolved first") };
}
