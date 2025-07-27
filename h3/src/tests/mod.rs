// This is to avoid an import loop:
// h3 tests depend on having private access to the crate.
// They must be part of the crate so as not to break privacy.
// They also depend on h3_quinn which depends on the crate.
// Having a dev-dependency on h3_quinn would work as far as cargo is
// concerned, but quic traits wouldn't match between the "h3" crate that
// comes before h3_quinn and the one that comes after and runs the tests
#[path = "../../../h3-quinn/src/lib.rs"]
mod h3_quinn;

mod connection;
mod request;

use std::{
    convert::TryInto,
    fmt::Debug,
    io::Cursor,
    net::{Ipv6Addr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

use bytes::{Buf, Bytes};
use http::Request;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::{
    qpack2::qpack_result::{ParseProgressResult, StatefulParser},
    quic,
};
use h3_quinn::{quinn::TransportConfig, Connection};

pub(crate) fn test_all_chunking_combinations<P, B, O, E>(
    input: &mut B,
    create: impl Fn() -> P,
    expected_output: ParseProgressResult<P, E, O>,
) where
    P: StatefulParser<E, O> + Debug,
    B: Buf,
    O: PartialEq + Debug,
    E: PartialEq + Debug,
{
    let mut data = vec![0; input.remaining()];
    input.copy_to_slice(data.as_mut_slice());

    let all_combinations = all_chunking_combinations(&data);

    for chunking in all_combinations {
        let mut parser = create();
        let mut chunks = chunking.iter();

        loop {
            let buf = chunks.next().unwrap().clone();
            let mut read = Cursor::new(&buf);

            parser = match parser.parse_progress(&mut read) {
                result @ ParseProgressResult::Error(_) => {
                    println!("Encountered error while parsing: {:?}", result);
                    assert_eq!(result, expected_output);
                    break;
                }
                result @ ParseProgressResult::Done(_) => {
                    println!("Parsing completed with result: {:?}", result);
                    assert_eq!(result, expected_output);
                    break;
                }
                ParseProgressResult::MoreData(p) => p,
            };
        }
    }
}

/// Generates all possible ways to chunk a buffer into a series of contiguous parts
/// Order is preserved and all elements appear exactly once across all chunks
pub(crate) fn all_chunking_combinations(buf: &[u8]) -> Vec<Vec<Vec<u8>>> {
    // Special case: empty buffer has only one way to chunk it - as empty
    if buf.is_empty() {
        return vec![vec![]];
    }

    // Special case: buffer of length 1 has only one chunking option
    if buf.len() == 1 {
        return vec![vec![buf.to_vec()]];
    }

    let mut result = Vec::new();

    // Consider all possible places to make the first cut
    for i in 1..=buf.len() {
        let first_chunk = buf[0..i].to_vec();

        // If this is the last possible cut (i == buf.len()), we're done
        if i == buf.len() {
            result.push(vec![first_chunk]);
            continue;
        }

        // Otherwise, recursively find all ways to chunk the remainder
        let remainder = &buf[i..];
        let remainder_chunking = all_chunking_combinations(remainder);

        // Combine the first chunk with each way to chunk the remainder
        for chunking in remainder_chunking {
            let mut new_chunking = vec![first_chunk.clone()];
            new_chunking.extend(chunking);
            result.push(new_chunking);
        }
    }

    result
}

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_test_writer()
        .try_init();
}

/// This accepts an incoming request. After the bidirectional stream is started it will not poll the
/// connection or receive further requests until the first headers are received.
/// Only use this for testing purposes.
async fn get_stream_blocking<C: quic::Connection<B>, B: Buf>(
    incoming: &mut crate::server::Connection<C, B>,
) -> Option<(Request<()>, crate::server::RequestStream<C::BidiStream, B>)> {
    let request_resolver = incoming.accept().await.ok()??;
    let (request, stream) = request_resolver.resolve_request().await.ok()?;
    Some((request, stream))
}

pub struct Pair {
    port: u16,
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    config: Arc<TransportConfig>,
}

impl Default for Pair {
    fn default() -> Self {
        let (cert, key) = build_certs();
        Self {
            cert,
            key,
            port: 0,
            config: Arc::new(TransportConfig::default()),
        }
    }
}

impl Pair {
    pub fn with_timeout(&mut self, duration: Duration) {
        Arc::get_mut(&mut self.config)
            .unwrap()
            .max_idle_timeout(Some(
                duration.try_into().expect("idle timeout duration invalid"),
            ))
            .initial_rtt(Duration::from_millis(10));
    }

    pub fn server_inner(&mut self) -> h3_quinn::Endpoint {
        let mut crypto = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![self.cert.clone()], self.key.clone_key())
        .unwrap();
        crypto.max_early_data_size = u32::MAX;
        crypto.alpn_protocols = vec![b"h3".to_vec()];

        let mut server_config = h3_quinn::quinn::ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).unwrap(),
        ));
        server_config.transport = self.config.clone();
        let endpoint =
            h3_quinn::quinn::Endpoint::server(server_config, "[::]:0".parse().unwrap()).unwrap();

        self.port = endpoint.local_addr().unwrap().port();

        endpoint
    }

    pub fn server(&mut self) -> Server {
        let endpoint = self.server_inner();
        Server { endpoint }
    }

    pub async fn client_inner(&self) -> quinn::Connection {
        let addr = (Ipv6Addr::LOCALHOST, self.port)
            .to_socket_addrs()
            .unwrap()
            .next()
            .unwrap();

        let mut root_cert_store = rustls::RootCertStore::empty();
        root_cert_store.add(self.cert.clone()).unwrap();
        let mut crypto = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(root_cert_store)
        .with_no_client_auth();
        crypto.enable_early_data = true;
        crypto.alpn_protocols = vec![b"h3".to_vec()];

        let client_config = h3_quinn::quinn::ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(crypto).unwrap(),
        ));

        let mut client_endpoint =
            h3_quinn::quinn::Endpoint::client("[::]:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(client_config);
        client_endpoint
            .connect(addr, "localhost")
            .unwrap()
            .await
            .unwrap()
    }

    pub async fn client(&self) -> h3_quinn::Connection {
        Connection::new(self.client_inner().await)
    }
}

pub struct Server {
    pub endpoint: h3_quinn::Endpoint,
}

impl Server {
    pub async fn next(&mut self) -> impl quic::Connection<Bytes> {
        Connection::new(self.endpoint.accept().await.unwrap().await.unwrap())
    }
}

pub fn build_certs() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    (
        cert.cert.into(),
        PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into()),
    )
}
