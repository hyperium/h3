use std::{path::PathBuf, sync::Arc};

use futures::future;
use structopt::StructOpt;
use tokio::io::AsyncWriteExt;
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
    //
    //             So we "move" it.
    //                  vvvv
    let request = async move {
        info!("sending request ...");

        let req = http::Request::builder().uri(uri).body(())?;

        // sending request results in a bidirectional stream,
        // which is also used for receiving response
        let mut stream = send_request.send_request(req).await?;

        // finish on the sending side
        stream.finish().await?;

        info!("receiving response ...");

        let resp = stream.recv_response().await?;

        info!("response: {:?} {}", resp.version(), resp.status());
        info!("headers: {:#?}", resp.headers());

        // `recv_data()` must be called after `recv_response()` for
        // receiving potential response body
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

    // wait for the connection to be closed before exiting
    client_endpoint.wait_idle().await;

    Ok(())
}
