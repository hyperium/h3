use anyhow::{Context, Result};
use bytes::Bytes;
use futures::StreamExt;
use h3::{
    ext::Protocol,
    quic::{self},
    server::Connection,
};
use h3_quinn::quinn::{self, crypto::rustls::QuicServerConfig};
use h3_webtransport::server::{self, IncomingStream, WebTransportConnection, WebTransportSession};
use http::Method;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use structopt::StructOpt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::pin;
use tracing::{error, info, trace_span};

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(
        short,
        long,
        default_value = "127.0.0.1:4433",
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
        default_value = "examples/localhost.crt",
        help = "Certificate for TLS. If present, `--key` is mandatory."
    )]
    pub cert: PathBuf,

    #[structopt(
        long,
        short,
        default_value = "examples/localhost.key",
        help = "Private key for the certificate."
    )]
    pub key: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 0. Setup tracing
    #[cfg(not(feature = "tree"))]
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .init();

    #[cfg(feature = "tree")]
    use tracing_subscriber::prelude::*;
    #[cfg(feature = "tree")]
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_tree::HierarchicalLayer::new(4).with_bracketed_fields(true))
        .init();

    // process cli arguments

    let opt = Opt::from_args();

    tracing::info!("Opt: {opt:#?}");
    let Certs { cert, key } = opt.certs;

    // create quinn server endpoint and bind UDP socket

    // both cert and key must be DER-encoded
    let cert = CertificateDer::from(std::fs::read(cert)?);
    let key = PrivateKeyDer::try_from(std::fs::read(key)?)?;

    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    tls_config.max_early_data_size = u32::MAX;
    let alpn: Vec<Vec<u8>> = vec![
        b"h3".to_vec(),
        b"h3-32".to_vec(),
        b"h3-31".to_vec(),
        b"h3-30".to_vec(),
        b"h3-29".to_vec(),
    ];
    tls_config.alpn_protocols = alpn;

    let mut server_config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.keep_alive_interval(Some(Duration::from_secs(2)));
    server_config.transport = Arc::new(transport_config);
    let endpoint = quinn::Endpoint::server(server_config, opt.listen)?;

    info!("listening on {}", opt.listen);

    // 2. Accept new quic connections and spawn a new task to handle them
    while let Some(new_conn) = endpoint.accept().await {
        trace_span!("New connection being attempted");

        tokio::spawn(async move {
            match new_conn.await {
                Ok(conn) => {
                    info!("new http3 established");
                    let h3_conn = h3::server::builder()
                        .enable_webtransport(true)
                        .enable_extended_connect(true)
                        .enable_datagram(true)
                        .max_webtransport_sessions(1)
                        .send_grease(true)
                        .build(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();

                    if let Err(err) = handle_connection(h3_conn).await {
                        tracing::error!("Failed to handle connection: {err:?}");
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

macro_rules! log_result {
    ($expr:expr) => {
        if let Err(err) = $expr {
            tracing::error!("{err:?}");
        }
    };
}

async fn handle_connection(conn: Connection<h3_quinn::Connection, Bytes>) -> Result<()> {
    type Session = WebTransportSession<h3_quinn::Connection, Bytes>;
    let mut driver = WebTransportConnection::new(conn);
    let mut datagrams = driver.datagram_reader();
    let mut resolving = futures::stream::FuturesUnordered::new();
    let mut accepting = futures::stream::FuturesUnordered::new();
    let mut tasks = tokio::task::JoinSet::new();
    let mut sessions: std::collections::HashMap<_, Arc<Session>> = std::collections::HashMap::new();

    loop {
        tokio::select! {
            biased;
            // Register completed sessions before processing their input streams.
            Some(session) = accepting.next() => {
                let session: Arc<Session> = Arc::new(session?);
                let id = session.session_id();
                sessions.insert(id, session.clone());
                tasks.spawn(async move {
                    open_bidi_test(session.open_bi().await?).await
                });
            }
            incoming = driver.accept() => match incoming? {
                Some(IncomingStream::Bidirectional(resolver)) => {
                    resolving.push(resolver.resolve());
                }
                Some(IncomingStream::Unidirectional(id, stream)) => {
                    if let Some(session) = sessions.get(&id).cloned() {
                        tasks.spawn(async move {
                            let send = session.open_uni().await?;
                            echo_stream(send, stream).await
                        });
                    }
                }
                None => break,
            },
            Some(resolved) = resolving.next() => match resolved? {
                server::AcceptedBi::Request(req, mut stream) => {
                    if req.method() == Method::CONNECT
                        && req.extensions().get::<Protocol>() == Some(&Protocol::WEB_TRANSPORT)
                    {
                        // The future owns only session resources; acceptance keeps running.
                        accepting.push(driver.accept_session(req, stream));
                    } else {
                        tasks.spawn(async move {
                            stream.send_response(http::Response::new(())).await?;
                            stream.finish().await?;
                            Ok(())
                        });
                    }
                }
                server::AcceptedBi::BidiStream(id, stream) => {
                    if sessions.contains_key(&id) {
                        let (send, recv) = quic::BidiStream::split(stream);
                        tasks.spawn(echo_stream(send, recv));
                    }
                }
            },
            datagram = datagrams.read_datagram() => {
                let datagram = datagram?;
                let id = datagram.stream_id().into();
                if let Some(session) = sessions.get(&id) {
                    session.send_datagram(datagram.into_payload())?;
                }
            }
            Some(result) = tasks.join_next() => {
                log_result!(result?);
            }
        }
    }
    while let Some(session) = accepting.next().await {
        let session = session?;
        sessions.insert(session.session_id(), Arc::new(session));
    }
    while let Some(result) = tasks.join_next().await {
        log_result!(result?);
    }
    Ok(())
}

async fn echo_stream<T, R>(send: T, recv: R) -> anyhow::Result<()>
where
    T: AsyncWrite,
    R: AsyncRead,
{
    pin!(send);
    pin!(recv);

    tracing::info!("Got stream");
    let mut buf = Vec::new();
    recv.read_to_end(&mut buf).await?;

    let message = Bytes::from(buf);

    send_chunked(send, message).await?;

    Ok(())
}

// Used to test that all chunks arrive properly as it is easy to write an impl which only reads and
// writes the first chunk.
async fn send_chunked(mut send: impl AsyncWrite + Unpin, data: Bytes) -> anyhow::Result<()> {
    for chunk in data.chunks(4) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        tracing::info!("Sending {chunk:?}");
        send.write_all(chunk).await?;
    }

    Ok(())
}

async fn open_bidi_test<S>(mut stream: S) -> anyhow::Result<()>
where
    S: Unpin + AsyncRead + AsyncWrite,
{
    tracing::info!("Opening bidirectional stream");

    stream
        .write_all(b"Hello from a server initiated bidi stream")
        .await
        .context("Failed to respond")?;

    let mut resp = Vec::new();
    stream.shutdown().await?;
    stream.read_to_end(&mut resp).await?;

    tracing::info!("Got response from client: {resp:?}");

    Ok(())
}
