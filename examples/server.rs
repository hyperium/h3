use std::path::PathBuf;

use futures::StreamExt;
use rustls::{Certificate, PrivateKey};
use structopt::StructOpt;
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tracing::{debug, error, info, trace, trace_span, warn};

// Configs for two server modes
// selfsigned mode will generate it's own local certificate
// certs mode will require a path to 2-3 files(cert, key, ca)
#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(subcommand)]
    pub command: Command,
}

#[derive(StructOpt, Debug)]
pub enum Command {
    #[structopt(name = "selfsigned")]
    SelfSigned(SelfSigned),

    #[structopt(name = "certs")]
    Certs(Certs),
}

#[derive(StructOpt, Debug)]
pub struct SelfSigned {
    #[structopt(long)]
    pub debug: bool,

    #[structopt(long, default_value = "4433")]
    pub port: u16,
}

#[derive(StructOpt, Debug)]
pub struct Certs {
    #[structopt(long)]
    pub cert: PathBuf,

    #[structopt(long)]
    pub key: PathBuf,

    #[structopt(long, default_value = "4433")]
    pub port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .init();

    let opt = Opt::from_args();
    trace!("{:#?}", opt);

    // quinn setup
    let (cert, key, port) = match opt.command {
        Command::SelfSigned(r) => {
            let (cert, key) = build_certs();
            (cert, key, r.port)
        }
        Command::Certs(c) => {
            let mut cert_v = Vec::new();
            let mut key_v = Vec::new();

            let mut cert_f = File::open(c.cert).await?;
            let mut key_f = File::open(c.key).await?;

            cert_f.read_to_end(&mut cert_v).await?;
            key_f.read_to_end(&mut key_v).await?;
            (rustls::Certificate(cert_v), PrivateKey(key_v), c.port)
        }
    };
    let server_config = h3_quinn::quinn::ServerConfig::with_single_cert(vec![cert], key)?;

    let addr = format!("[::]:{:}", port).parse()?;
    let (endpoint, mut incoming) = h3_quinn::quinn::Endpoint::server(server_config, addr)?;

    info!(
        "Listening on port {:?}",
        endpoint.local_addr().unwrap().port()
    );

    while let Some(new_conn) = incoming.next().await {
        trace_span!("New connection being attempted");

        tokio::spawn(async move {
            match new_conn.await {
                Ok(conn) => {
                    debug!("New connection now established");

                    let mut h3_conn = h3::server::Connection::new(h3_quinn::Connection::new(conn))
                        .await
                        .unwrap();

                    while let Some((req, mut stream)) = h3_conn.accept().await.unwrap() {
                        debug!("connection requested: {:#?}", req);

                        tokio::spawn(async move {
                            let resp = http::Response::builder()
                                .status(http::StatusCode::NOT_FOUND)
                                .body(())
                                .unwrap();

                            match stream.send_response(resp).await {
                                Ok(_) => {
                                    debug!("Response to connection successful");
                                }
                                Err(err) => {
                                    error!("Unable to send response to connection peer: {:?}", err);
                                }
                            }

                            stream.finish().await.unwrap();
                        });
                    }
                }
                Err(err) => {
                    warn!("connecting client failed with error: {:?}", err);
                }
            }
        });
    }

    Ok(())
}

pub fn build_certs() -> (Certificate, PrivateKey) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivateKey(cert.serialize_private_key_der());
    let cert = Certificate(cert.serialize_der().unwrap());
    (cert, key)
}
