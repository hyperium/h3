use std::sync::Arc;
use std::time::SystemTime;

use rustls;
use rustls::{Certificate, ServerName};
use rustls::client::ServerCertVerified;
use structopt::StructOpt;
use tokio::io::AsyncWriteExt;

use h3_quinn::quinn;
use h3_quinn::quinn::crypto::rustls::Error;

static ALPN: &[u8] = b"h3-29";

#[derive(StructOpt, Debug)]
#[structopt(name = "server")]
struct Opt {
    #[structopt(long)]
    pub insecure: bool,

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

    let client_config = if !opt.insecure {
        h3_quinn::quinn::ClientConfig::with_native_roots()
    } else {
        let mut tls_config = rustls::ClientConfig::builder()
            .with_safe_default_cipher_suites()
            .with_safe_default_kx_groups()
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_custom_certificate_verifier(Arc::new(YesVerifier))
            .with_no_client_auth();
        tls_config.enable_early_data = true;
        tls_config.alpn_protocols = vec![ALPN.into()];
        let transport = quinn::TransportConfig::default();
        quinn::ClientConfig {
            crypto: Arc::new(tls_config),
            transport: Arc::new(transport),
        }
    };

    let mut client_endpoint = h3_quinn::quinn::Endpoint::client("[::]:0".parse().unwrap())?;
    client_endpoint.set_default_client_config(client_config);
    let quinn_conn = h3_quinn::Connection::new(client_endpoint.connect(addr, auth.host())?.await?);

    eprintln!("QUIC connected ...");

    // generic h3
    let mut conn = h3::client::Connection::new(quinn_conn).await?;

    eprintln!("Sending request ...");

    // send a request!
    let req = http::Request::builder().uri(dest).body(())?;
    let mut stream = conn.send_request(req).await?;

    // no req body!
    stream.finish().await?;

    eprintln!("Receiving response ...");

    let resp = stream.recv_response().await?;

    eprintln!("Response: {:?} {}", resp.version(), resp.status());
    eprintln!("Headers: {:#?}", resp.headers());

    while let Some(chunk) = stream.recv_data().await? {
        let mut out = tokio::io::stdout();
        out.write_all(&chunk).await?;
        out.flush().await?;
    }

    Ok(())
}

struct YesVerifier;

impl rustls::client::ServerCertVerifier for YesVerifier {
    fn verify_server_cert(&self, _end_entity: &Certificate,
                          _intermediates: &[Certificate],
                          _server_name: &ServerName,
                          _scts: &mut dyn Iterator<Item=&[u8]>,
                          _ocsp_response: &[u8],
                          _now: SystemTime) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }
}
