use std::sync::Arc;
use std::time::SystemTime;

use futures::future;
use h3_quinn::quinn;
use rustls::{self, client::ServerCertVerified};
use rustls::{Certificate, ServerName};
use structopt::StructOpt;
use tokio::{self, io::AsyncWriteExt};

use h3_quinn::{self, quinn::crypto::rustls::Error};

static ALPN: &[u8] = b"h3";

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(long)]
    pub insecure: bool,

    #[structopt(name = "keylogfile", long)]
    pub key_log_file: bool,

    #[structopt()]
    pub uri: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .init();

    let opt = Opt::from_args();

    let dest = opt.uri.parse::<http::Uri>()?;

    if dest.scheme() != Some(&http::uri::Scheme::HTTPS) {
        Err("destination scheme must be 'https'")?;
    }

    let auth = dest
        .authority()
        .ok_or("destination must have a host")?
        .clone();

    let port = auth.port_u16().unwrap_or(443);

    // dns me!
    let addr = tokio::net::lookup_host((auth.host(), port))
        .await?
        .next()
        .ok_or("dns found no addresses")?;

    eprintln!("DNS Lookup for {:?}: {:?}", dest, addr);

    // quinn setup
    let tls_config_builder = rustls::ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_protocol_versions(&[&rustls::version::TLS13])?;
    let mut tls_config = if !opt.insecure {
        let mut roots = rustls::RootCertStore::empty();
        match rustls_native_certs::load_native_certs() {
            Ok(certs) => {
                for cert in certs {
                    if let Err(e) = roots.add(&rustls::Certificate(cert.0)) {
                        eprintln!("failed to parse trust anchor: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("couldn't load any default trust roots: {}", e);
            }
        };
        tls_config_builder
            .with_root_certificates(roots)
            .with_no_client_auth()
    } else {
        tls_config_builder
            .with_custom_certificate_verifier(Arc::new(YesVerifier))
            .with_no_client_auth()
    };
    tls_config.enable_early_data = true;
    tls_config.alpn_protocols = vec![ALPN.into()];

    if opt.key_log_file {
        // Write all Keys to a file if SSLKEYLOGFILE is set
        // WARNING, we enable this for the example, you should think carefully about enabling in your own code
        tls_config.key_log = Arc::new(rustls::KeyLogFile::new());
    }

    let client_config = quinn::ClientConfig::new(Arc::new(tls_config));

    let mut client_endpoint = h3_quinn::quinn::Endpoint::client("[::]:0".parse().unwrap())?;
    client_endpoint.set_default_client_config(client_config);
    let quinn_conn = h3_quinn::Connection::new(client_endpoint.connect(addr, auth.host())?.await?);

    eprintln!("QUIC connected ...");

    // generic h3
    let (mut driver, mut send_request) = h3::client::new(quinn_conn).await?;

    let drive = async move {
        driver.poll_close().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    // In the following block, we want to take ownership of `send_request`:
    // the connection will be closed only when all `SendRequest`s instances
    // are dropped.
    //
    //             So we "move" it.
    //                  vvvv
    let request = async move {
        eprintln!("Sending request ...");

        let req = http::Request::builder().uri(dest).body(())?;

        let mut stream = send_request.send_request(req).await?;
        stream.finish().await?;

        eprintln!("Receiving response ...");
        let resp = stream.recv_response().await?;

        eprintln!("Response: {:?} {}", resp.version(), resp.status());
        eprintln!("Headers: {:#?}", resp.headers());

        while let Some(mut chunk) = stream.recv_data().await? {
            let mut out = tokio::io::stdout();
            out.write_all_buf(&mut chunk).await?;
            out.flush().await?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    };

    let (req_res, drive_res) = tokio::join!(request, drive);
    req_res?;
    drive_res?;

    client_endpoint.wait_idle().await;

    Ok(())
}

struct YesVerifier;

impl rustls::client::ServerCertVerifier for YesVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &Certificate,
        _intermediates: &[Certificate],
        _server_name: &ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: SystemTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }
}
