use anyhow::{Context, Result};
use bytes::Bytes;
use h3::{
    ext::Protocol,
    quic::{self},
    server::Connection,
};
use h3_quinn::quinn::{self, crypto::rustls::QuicServerConfig};
use h3_webtransport::server::{self, WebTransportSession};
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

                    // tracing::info!("Establishing WebTransport session");
                    // // 3. TODO: Conditionally, if the client indicated that this is a webtransport session, we should accept it here, else use regular h3.
                    // // if this is a webtransport session, then h3 needs to stop handing the datagrams, bidirectional streams, and unidirectional streams and give them
                    // // to the webtransport session.

                    if let Err(err) = handle_connection(h3_conn).await {
                        tracing::error!("Failed to handle connection: {err:?}");
                    }

                    // let mut session: WebTransportSession<_, Bytes> =
                    //     WebTransportSession::accept(h3_conn).await.unwrap();
                    // tracing::info!("Finished establishing webtransport session");
                    // // 4. Get datagrams, bidirectional streams, and unidirectional streams and wait for client requests here.
                    // // h3_conn needs to hand over the datagrams, bidirectional streams, and unidirectional streams to the webtransport session.
                    // let result = handle.await;
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

async fn handle_connection(mut conn: Connection<h3_quinn::Connection, Bytes>) -> Result<()> {
    // 3. TODO: Conditionally, if the client indicated that this is a webtransport session, we should accept it here, else use regular h3.
    // if this is a webtransport session, then h3 needs to stop handing the datagrams, bidirectional streams, and unidirectional streams and give them
    // to the webtransport session.

    loop {
        match conn.accept().await {
            Ok(Some(resolver)) => {
                // TODO: resolve request in a different task to not block the accept loop
                let (req, stream) = match resolver.resolve_request().await {
                    Ok(request) => request,
                    Err(err) => {
                        error!("error resolving request: {err:?}");
                        continue;
                    }
                };
                info!("new request: {:#?}", req);

                let ext = req.extensions();
                match req.method() {
                    &Method::CONNECT if ext.get::<Protocol>() == Some(&Protocol::WEB_TRANSPORT) => {
                        tracing::info!("Peer wants to initiate a webtransport session");

                        tracing::info!("Handing over connection to WebTransport");
                        let session = WebTransportSession::accept(req, stream, conn).await?;
                        tracing::info!("Established webtransport session");
                        // 4. Get datagrams, bidirectional streams, and unidirectional streams and wait for client requests here.
                        // h3_conn needs to hand over the datagrams, bidirectional streams, and unidirectional streams to the webtransport session.
                        handle_session_and_echo_all_inbound_messages(session).await?;

                        return Ok(());
                    }
                    _ => {
                        tracing::info!(?req, "Received request");
                    }
                }
            }
            // indicating no more streams to be received
            Ok(None) => {
                break;
            }
            Err(err) => {
                error!("Connection errored with {}", err);
                break;
            }
        }
    }
    Ok(())
}

macro_rules! log_result {
    ($expr:expr) => {
        if let Err(err) = $expr {
            tracing::error!("{err:?}");
        }
    };
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

/// This method will echo all inbound datagrams, unidirectional and bidirectional streams.
async fn handle_session_and_echo_all_inbound_messages(
    session: WebTransportSession<h3_quinn::Connection, Bytes>,
) -> anyhow::Result<()> {
    let session_id = session.session_id();

    // This will open a bidirectional stream and send a message to the client right after connecting!
    let stream = session.open_bi(session_id).await?;

    tokio::spawn(async move { log_result!(open_bidi_test(stream).await) });

    let mut datagram_reader = session.datagram_reader();
    let mut datagram_sender = session.datagram_sender();

    loop {
        tokio::select! {
            datagram = datagram_reader.read_datagram() => {
                let datagram = match datagram {
                    Ok(datagram) => datagram,
                    Err(err) => {
                        tracing::error!("Failed to read datagram: {err:?}");
                        break;
                    }
                };
                tracing::info!("Received datagram: {datagram:?}");
                let datagram = datagram.into_payload();
                datagram_sender.send_datagram(datagram)?;
            }
            uni_stream = session.accept_uni() => {
                let (id, stream) = uni_stream?.unwrap();

                let send = session.open_uni(id).await?;
                tokio::spawn( async move { log_result!(echo_stream(send, stream).await); });
            }
            stream = session.accept_bi() => {
                if let Some(server::AcceptedBi::BidiStream(_, stream)) = stream? {
                    let (send, recv) = quic::BidiStream::split(stream);
                    tokio::spawn( async move { log_result!(echo_stream(send, recv).await); });
                }
            }
            else => {
                break
            }
        }
    }

    tracing::info!("Finished handling session");

    Ok(())
}
