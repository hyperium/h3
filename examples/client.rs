use std::sync::{Arc, Mutex};

use h3_quinn::quinn;
use rustls;
use structopt::StructOpt;
use tokio::io::AsyncWriteExt;
use webpki;

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
    let mut client_config = h3_quinn::quinn::ClientConfigBuilder::default();
    client_config.protocols(&[ALPN]);

    let mut client_endpoint_builder = h3_quinn::quinn::Endpoint::builder();

    if !opt.insecure {
        client_endpoint_builder.default_client_config(client_config.build());
    } else {
        let mut tls_config = rustls::ClientConfig::new();
        tls_config.versions = vec![rustls::ProtocolVersion::TLSv1_3];
        tls_config.enable_early_data = true;
        tls_config
            .dangerous()
            .set_certificate_verifier(Arc::new(YesVerifier));
        tls_config.alpn_protocols = vec![ALPN.into()];

        let transport = quinn::TransportConfig::default();
        client_endpoint_builder.default_client_config(quinn::ClientConfig {
            crypto: Arc::new(tls_config),
            transport: Arc::new(transport),
        });
    }

    let (client_endpoint, _) = client_endpoint_builder
        .bind(&"[::]:0".parse().unwrap())
        .unwrap();

    let quinn_conn = h3_quinn::Connection::new(client_endpoint.connect(&addr, auth.host())?.await?);

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
impl rustls::ServerCertVerifier for YesVerifier {
    fn verify_server_cert(
        &self,
        _roots: &rustls::RootCertStore,
        _presented_certs: &[rustls::Certificate],
        _dns_name: webpki::DNSNameRef,
        _ocsp_response: &[u8],
    ) -> std::result::Result<rustls::ServerCertVerified, rustls::TLSError> {
        Ok(rustls::ServerCertVerified::assertion())
    }
}
