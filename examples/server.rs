use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http::{Request, StatusCode};
use rustls::{Certificate, PrivateKey};
use structopt::StructOpt;
use tokio::{fs::File, io::AsyncReadExt};
use tracing::{debug, error, info, trace_span, warn};

use h3::{quic::BidiStream, server::RequestStream, error::ErrorLevel};

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(
        name = "dir",
        short,
        long,
        help = "Root directory of the files to serve. \
                If omitted, server will respond OK."
    )]
    pub root: Option<PathBuf>,

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
        help = "Certificate for TLS. \
                If present, `--key` is mandatory. \
                If omitted, a selfsigned certificate will be generated."
    )]
    pub cert: Option<PathBuf>,

    #[structopt(long, short, help = "Private key for the certificate.")]
    pub key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .init();

    let opt = Opt::from_args();

    let root = if let Some(root) = opt.root {
        if !root.is_dir() {
            return Err(format!("{}: is not a readable directory", root.display()).into());
        } else {
            info!("serving {}", root.display());
            Arc::new(Some(root))
        }
    } else {
        Arc::new(None)
    };

    let crypto = load_crypto(opt.certs).await?;
    let server_config = h3_quinn::quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let (endpoint, mut incoming) = h3_quinn::quinn::Endpoint::server(server_config, opt.listen)?;

    info!("Listening on {}", opt.listen);

    while let Some(new_conn) = incoming.next().await {
        trace_span!("New connection being attempted");

        let root = root.clone();
        tokio::spawn(async move {
            match new_conn.await {
                Ok(conn) => {
                    debug!("New connection now established");

                    let mut h3_conn = h3::server::Connection::new(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();
                    loop {
                        match h3_conn.accept().await {
                            Ok(Some((req, stream))) => {
                                let root = root.clone();
                                debug!("New request: {:#?}", req);

                                tokio::spawn(async {
                                    if let Err(e) = handle_request(req, stream, root).await {
                                        error!("request failed: {}", e);
                                    }
                                });
                            }
                            Ok(None) => {
                                break;
                            }
                            Err(err) => match err.kind() {
                                h3::error::Kind::Application {
                                    code: _,
                                    reason: _,
                                    level: ErrorLevel::ConnectionError,
                                    ..
                                } => {
                                    warn!("Error on accept, {}", err);
                                    break;
                                }
                                h3::error::Kind::Application {
                                    code: _,
                                    reason: _,
                                    level: ErrorLevel::StreamError,
                                    ..
                                } => {
                                    warn!("Error on accept, {}", err);
                                    continue;
                                }
                                h3::error::Kind::HeaderTooBig { .. } => continue,
                                _ => {
                                    error!("Error on accept: {}", err);
                                    break;
                                }
                            },
                        }
                    }
                }
                Err(err) => {
                    warn!("accepting connection failed: {:?}", err);
                }
            }
        });
    }

    endpoint.wait_idle().await;

    Ok(())
}

async fn handle_request<T>(
    req: Request<()>,
    mut stream: RequestStream<T, Bytes>,
    serve_root: Arc<Option<PathBuf>>,
) -> Result<(), Box<dyn std::error::Error>>
where
    T: BidiStream<Bytes>,
{
    let (status, to_serve) = match serve_root.as_deref() {
        None => (StatusCode::OK, None),
        Some(_) if req.uri().path().contains("..") => (StatusCode::NOT_FOUND, None),
        Some(root) => {
            let to_serve = root.join(req.uri().path().strip_prefix("/").unwrap_or(""));
            match File::open(&to_serve).await {
                Ok(file) => (StatusCode::OK, Some(file)),
                Err(e) => {
                    debug!("failed to open: \"{}\": {}", to_serve.to_string_lossy(), e);
                    (StatusCode::NOT_FOUND, None)
                }
            }
        }
    };

    let resp = http::Response::builder().status(status).body(()).unwrap();

    match stream.send_response(resp).await {
        Ok(_) => {
            debug!("Response to connection successful");
        }
        Err(err) => {
            error!("Unable to send response to connection peer: {:?}", err);
        }
    }

    if let Some(mut file) = to_serve {
        loop {
            let mut buf = BytesMut::with_capacity(4096 * 10);
            if file.read_buf(&mut buf).await? == 0 {
                break;
            }
            stream.send_data(buf.freeze()).await?;
        }
    }

    Ok(stream.finish().await?)
}

static ALPN: &[u8] = b"h3";

async fn load_crypto(opt: Certs) -> Result<rustls::ServerConfig, Box<dyn std::error::Error>> {
    let (cert, key) = match (opt.cert, opt.key) {
        (None, None) => build_certs(),
        (Some(cert_path), Some(ref key_path)) => {
            let mut cert_v = Vec::new();
            let mut key_v = Vec::new();

            let mut cert_f = File::open(cert_path).await?;
            let mut key_f = File::open(key_path).await?;

            cert_f.read_to_end(&mut cert_v).await?;
            key_f.read_to_end(&mut key_v).await?;
            (rustls::Certificate(cert_v), PrivateKey(key_v))
        }
        (_, _) => return Err("cert and key args are mutually dependant".into()),
    };

    let mut crypto = rustls::ServerConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    crypto.max_early_data_size = u32::MAX;
    crypto.alpn_protocols = vec![ALPN.into()];

    Ok(crypto)
}

pub fn build_certs() -> (Certificate, PrivateKey) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivateKey(cert.serialize_private_key_der());
    let cert = Certificate(cert.serialize_der().unwrap());
    (cert, key)
}
