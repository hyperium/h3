use futures::{future, FutureExt};
use tokio::{self, io::AsyncWriteExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_writer(std::io::stderr)
        .init();

    let dest = std::env::args()
        .nth(1)
        .ok_or("must include destination argument")?;

    let dest = dest.parse::<http::Uri>()?;

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
    client_config.protocols(&[b"h3-29"]);

    let mut client_endpoint_builder = h3_quinn::quinn::Endpoint::builder();
    client_endpoint_builder.default_client_config(client_config.build());
    let (client_endpoint, _) = client_endpoint_builder
        .bind(&"[::]:0".parse().unwrap())
        .unwrap();

    let quinn_conn = h3_quinn::Connection::new(client_endpoint.connect(&addr, auth.host())?.await?);

    eprintln!("QUIC connected ...");

    // generic h3
    let (mut driver, mut conn) = h3::client::new(quinn_conn).await?;

    let drive = async move {
        future::poll_fn(|cx| driver.poll_close(cx)).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .fuse();

    let request = async {
        eprintln!("Sending request ...");

        let req = http::Request::builder().uri(dest).body(())?;

        let mut stream = conn.send_request(req).await?;
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
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    tokio::select! {
        _ = request => {
            println!("request completed");
        }
        _ = drive => {
            println!("operation compleed");
        }
    }

    Ok(())
}
