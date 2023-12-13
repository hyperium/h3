use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use bytes::{Buf, Bytes, BytesMut};
use http::StatusCode;
use rustls::{Certificate, PrivateKey};
use structopt::StructOpt;
use tokio::io::AsyncReadExt;
use tracing::{error, info, trace_span};

use h3::{error::ErrorLevel, server::RequestStream};
use h3_quinn::quinn;

use std::io::ErrorKind;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::ReadBuf;

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(
        short,
        long,
        default_value = "[::1]:4433",
        help = "What address:port to listen for new connections"
    )]
    pub listen: SocketAddr,

    #[structopt(flatten)]
    pub certs: Certs,
}

#[derive(StructOpt, Debug)]
pub struct Certs {
    #[structopt(
        long,
        short,
        default_value = "examples/server.cert",
        help = "Certificate for TLS. If present, `--key` is mandatory."
    )]
    pub cert: PathBuf,

    #[structopt(
        long,
        short,
        default_value = "examples/server.key",
        help = "Private key for the certificate."
    )]
    pub key: PathBuf,
}

static ALPN: &[u8] = b"h3";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    // process cli arguments

    let opt = Opt::from_args();

    let Certs { cert, key } = opt.certs;

    // create quinn server endpoint and bind UDP socket

    // both cert and key must be DER-encoded
    let cert = Certificate(std::fs::read(cert)?);
    let key = PrivateKey(std::fs::read(key)?);

    let mut tls_config = rustls::ServerConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    tls_config.max_early_data_size = u32::MAX;
    tls_config.alpn_protocols = vec![ALPN.into()];

    let server_config = quinn::ServerConfig::with_crypto(Arc::new(tls_config));
    let endpoint = quinn::Endpoint::server(server_config, opt.listen)?;

    info!("listening on {}", opt.listen);

    // handle incoming connections and requests

    while let Some(new_conn) = endpoint.accept().await {
        trace_span!("New connection being attempted");

        tokio::spawn(async move {
            match new_conn.await {
                Ok(conn) => {
                    info!("new connection established");

                    let mut h3_conn = h3::server::Connection::new(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();

                    loop {
                        match h3_conn.accept().await {
                            Ok(Some((req, stream))) => {
                                info!("new request: {:#?}", req);

                                tokio::spawn(async move {
                                    if let Err(e) = handle_request(stream).await {
                                        error!("handling request failed: {}", e);
                                    }
                                });
                            }

                            // indicating no more streams to be received
                            Ok(None) => {
                                break;
                            }

                            Err(err) => {
                                error!("error on accept {}", err);
                                match err.get_error_level() {
                                    ErrorLevel::ConnectionError => break,
                                    ErrorLevel::StreamError => continue,
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    error!("accepting connection failed: {:?}", err);
                }
            }
        });
    }

    // shut down gracefully
    // wait for connections to be closed before exiting
    endpoint.wait_idle().await;

    Ok(())
}

pub struct Streaming {
    stream: RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
    pending_read: Bytes,
    pending_write: bool,
}

impl Streaming {
    pub(crate) fn new(stream: RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>) -> Self {
        Self {
            stream,
            pending_read: Bytes::new(),
            pending_write: false,
        }
    }

    fn copy_from_pending(&mut self, copy_to: &mut ReadBuf<'_>) {
        let copy = std::cmp::min(copy_to.remaining(), self.pending_read.len());
        copy_to.put_slice(&self.pending_read[0..copy]);
        self.pending_read.advance(copy);
    }
}

fn to_std_err(e: h3::error::Error) -> std::io::Error {
    std::io::Error::new(ErrorKind::Other, e)
}

impl AsyncWrite for Streaming {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        if self.pending_write {
            return match self.stream.poll_ready(cx) {
                Poll::Ready(Ok(())) => {
                    self.pending_write = false;
                    Poll::Ready(Ok(buf.len()))
                }
                Poll::Ready(Err(err)) => Poll::Ready(Err(to_std_err(err))),
                Poll::Pending => Poll::Pending,
            };
        }
        if let Err(err) = self.stream.push_data(Bytes::copy_from_slice(buf)) {
            return Poll::Ready(Err(to_std_err(err)));
        }
        match self.stream.poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(buf.len())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(to_std_err(err))),
            Poll::Pending => {
                self.pending_write = true;
                Poll::Pending
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        match self.stream.poll_shutdown(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(to_std_err(err))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncRead for Streaming {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.pending_read.is_empty() {
            self.copy_from_pending(buf);
            return Poll::Ready(Ok(()));
        }

        match self.stream.poll_read(cx) {
            Poll::Ready(Ok(Some(data))) => {
                self.pending_read = Bytes::copy_from_slice(data.chunk());
                self.copy_from_pending(buf);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(None)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(e)) => Poll::Ready(Err(to_std_err(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn echo_service(mut streaming: Streaming) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let mut buf = BytesMut::with_capacity(32);
        if let Err(err) = streaming.read_buf(&mut buf).await {
            info!(?err, "read error");
            return Err(err.into());
        }
        info!("data from client {:?}", String::from_utf8_lossy(&buf[..]));
        if let Err(err) = streaming.write_all(&buf).await {
            info!(?err, "write error");
            return Err(err.into());
        }
    }
}

async fn handle_request(
    mut stream: RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>,
) -> Result<(), Box<dyn std::error::Error>> {
    let resp = http::Response::builder()
        .status(StatusCode::OK)
        .body(())
        .unwrap();

    match stream.send_response(resp).await {
        Ok(_) => {
            info!("successfully respond to connection");
        }
        Err(err) => {
            error!("unable to send response to connection peer: {:?}", err);
        }
    }

    let streaming = Streaming::new(stream);
    echo_service(streaming).await
}
