use std::{path::PathBuf, sync::Arc, time::Duration};

use bytes::Bytes;
use bytes::{Buf, BytesMut};
use futures::future;
use h3::client::RequestStream;
use std::io::ErrorKind;
use std::pin::Pin;
use std::task::{Context, Poll};
use structopt::StructOpt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::ReadBuf;
use tokio::io::{AsyncRead, AsyncReadExt};
use tracing::{error, info};

use h3_quinn::quinn;

static ALPN: &[u8] = b"h3";

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(
        long,
        short,
        default_value = "examples/ca.cert",
        help = "Certificate of CA who issues the server certificate"
    )]
    pub ca: PathBuf,

    #[structopt(name = "keylogfile", long)]
    pub key_log_file: bool,

    #[structopt()]
    pub uri: String,
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

async fn tx_write(mut tx: tokio::io::WriteHalf<Streaming>) {
    let mut next = 0;
    loop {
        let send = format!("Client sequence {next}");
        next += 1;
        if let Err(err) = tx.write_all(send.as_bytes()).await {
            info!(?err, "read error");
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn rx_read(mut rx: tokio::io::ReadHalf<Streaming>) {
    loop {
        let mut buf = BytesMut::with_capacity(32);
        if let Err(err) = rx.read_buf(&mut buf).await {
            info!(?err, "read error");
            break;
        }
        info!("data from server {:?}\n", String::from_utf8_lossy(&buf[..]));
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    let opt = Opt::from_args();

    // DNS lookup

    let uri = opt.uri.parse::<http::Uri>()?;

    if uri.scheme() != Some(&http::uri::Scheme::HTTPS) {
        Err("uri scheme must be 'https'")?;
    }

    let auth = uri.authority().ok_or("uri must have a host")?.clone();

    let port = auth.port_u16().unwrap_or(443);

    let addr = tokio::net::lookup_host((auth.host(), port))
        .await?
        .next()
        .ok_or("dns found no addresses")?;

    info!("DNS lookup for {:?}: {:?}", uri, addr);

    // create quinn client endpoint

    // load CA certificates stored in the system
    let mut roots = rustls::RootCertStore::empty();
    match rustls_native_certs::load_native_certs() {
        Ok(certs) => {
            for cert in certs {
                if let Err(e) = roots.add(&rustls::Certificate(cert.0)) {
                    error!("failed to parse trust anchor: {}", e);
                }
            }
        }
        Err(e) => {
            error!("couldn't load any default trust roots: {}", e);
        }
    };

    // load certificate of CA who issues the server certificate
    // NOTE that this should be used for dev only
    if let Err(e) = roots.add(&rustls::Certificate(std::fs::read(opt.ca)?)) {
        error!("failed to parse trust anchor: {}", e);
    }

    let mut tls_config = rustls::ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();

    tls_config.enable_early_data = true;
    tls_config.alpn_protocols = vec![ALPN.into()];

    // optional debugging support
    if opt.key_log_file {
        // Write all Keys to a file if SSLKEYLOGFILE is set
        // WARNING, we enable this for the example, you should think carefully about enabling in your own code
        tls_config.key_log = Arc::new(rustls::KeyLogFile::new());
    }

    let mut client_endpoint = h3_quinn::quinn::Endpoint::client("[::]:0".parse().unwrap())?;

    let client_config = quinn::ClientConfig::new(Arc::new(tls_config));
    client_endpoint.set_default_client_config(client_config);

    let conn = client_endpoint.connect(addr, auth.host())?.await?;

    info!("QUIC connection established");

    // create h3 client

    // h3 is designed to work with different QUIC implementations via
    // a generic interface, that is, the [`quic::Connection`] trait.
    // h3_quinn implements the trait w/ quinn to make it work with h3.
    let quinn_conn = h3_quinn::Connection::new(conn);

    let (mut driver, mut send_request) = h3::client::new(quinn_conn).await?;

    let drive = async move {
        future::poll_fn(|cx| driver.poll_close(cx)).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    // In the following block, we want to take ownership of `send_request`:
    // the connection will be closed only when all `SendRequest`s instances
    // are dropped.
    let request = async move {
        info!("sending request ...");
        let req = http::Request::builder().uri(uri).body(())?;
        let mut stream = send_request.send_request(req).await?;
        info!("waiting for response ...");
        let resp = stream.recv_response().await?;
        info!("response: {:?}", resp);

        // Now we convert the stream into a bidirectional stream
        // of bytes which are AsyncRead/AsyncWrite
        let streaming = Streaming::new(stream);
        let (rx, tx) = tokio::io::split(streaming);

        tokio::select! {
            _ = tx_write(tx) => {},
            _ = rx_read(rx) => {}
        }

        Ok::<_, Box<dyn std::error::Error>>(())
    };

    let (req_res, drive_res) = tokio::join!(request, drive);
    req_res?;
    drive_res?;

    // wait for the connection to be closed before exiting
    client_endpoint.wait_idle().await;

    Ok(())
}
