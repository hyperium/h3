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
    use_sampled: bool,
    expected_output: ParseProgressResult<P, E, O>,
) where
    P: StatefulParser<E, O> + Debug,
    B: Buf,
    O: PartialEq + Debug,
    E: PartialEq + Debug,
{
    let mut data = vec![0; input.remaining()];
    input.copy_to_slice(data.as_mut_slice());

    let all_combinations = if use_sampled {
        sampled_chunking_combinations(&data)
    } else {
        all_chunking_combinations(&data)
    };

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

/// Produces a deterministic subset of chunking combinations for `buf`,
/// always returning fewer than 100 combinations while preserving order.
///
/// Rationale: `all_chunking_combinations` is exponential (2^(n-1)) and becomes
/// infeasible for large inputs. This function returns a representative, stable
/// set of chunkings to exercise stateful parsers in tests without blowing up
/// runtime.
pub(crate) fn sampled_chunking_combinations(buf: &[u8]) -> Vec<Vec<Vec<u8>>> {
    use std::collections::HashSet;

    const LIMIT: usize = 96; // strictly < 100

    // Helpers
    fn push_if_new(
        out: &mut Vec<Vec<Vec<u8>>>,
        seen: &mut HashSet<Vec<usize>>,
        chunks: Vec<Vec<u8>>,
    ) {
        // Build a signature based on chunk sizes to avoid duplicates.
        let sig: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
        if seen.insert(sig) {
            out.push(chunks);
        }
    }

    fn by_fixed_stride(buf: &[u8], stride: usize) -> Vec<Vec<u8>> {
        let buffer_length = buf.len();
        if stride == 0 || buffer_length == 0 {
            return vec![buf.to_vec()];
        }
        let mut chunks = Vec::new();
        let mut counter = 0;
        while counter < buffer_length {
            let end = (counter + stride).min(buffer_length);
            chunks.push(buf[counter..end].to_vec());
            counter = end;
        }
        chunks
    }

    fn by_sizes(buf: &[u8], sizes: &[usize]) -> Vec<Vec<u8>> {
        let buffer_length = buf.len();
        if buffer_length == 0 {
            return vec![];
        }
        let mut chunks = Vec::new();
        let mut lower_end = 0;
        for &stride_length in sizes {
            if lower_end >= buffer_length {
                break;
            }
            let upper_end = (lower_end + stride_length).min(buffer_length);
            chunks.push(buf[lower_end..upper_end].to_vec());
            lower_end = upper_end;
        }
        if lower_end < buffer_length {
            chunks.push(buf[lower_end..buffer_length].to_vec());
        }
        chunks
    }

    let len = buf.len();
    // Small inputs: just return all combinations for clarity/convenience.
    if len <= 10 {
        return all_chunking_combinations(buf);
    }

    let mut out: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut seen: HashSet<Vec<usize>> = HashSet::new();

    // 1) Extremes
    // single chunk
    push_if_new(&mut out, &mut seen, vec![buf.to_vec()]);
    if out.len() >= LIMIT {
        return out;
    }
    // all single-byte chunks
    push_if_new(&mut out, &mut seen, by_fixed_stride(buf, 1));
    if out.len() >= LIMIT {
        return out;
    }

    // 2) Equal partitions for k in a curated set (favoring variety)
    let k_values = [2usize, 3, 4, 5, 6, 7, 8, 9, 10, 12, 16, 24, 32];
    for &k in &k_values {
        if k > len || out.len() >= LIMIT {
            continue;
        }
        // Split into roughly equal parts
        let base = len / k;
        let rem = len % k;
        let mut sizes = Vec::with_capacity(k);
        for i in 0..k {
            let s = base + if i < rem { 1 } else { 0 };
            if s == 0 {
                break;
            }
            sizes.push(s);
        }
        if !sizes.is_empty() {
            let chunks = by_sizes(buf, &sizes);
            push_if_new(&mut out, &mut seen, chunks);
            if out.len() >= LIMIT {
                return out;
            }
        }
    }

    // 3) Fixed strides (powers of two and some primes) to create regular boundaries
    let mut strides = vec![2usize, 3, 4, 5, 7, 8, 11, 13, 16, 19, 23, 29, 32, 37, 43, 47, 53, 64];
    strides.retain(|&s| s <= len);
    strides.sort_unstable();
    for s in strides {
        if out.len() >= LIMIT {
            break;
        }
        let chunks = by_fixed_stride(buf, s);
        push_if_new(&mut out, &mut seen, chunks);
    }

    if out.len() >= LIMIT {
        return out;
    }

    // 4) Alternating patterns to simulate uneven IO
    let patterns: &[&[usize]] = &[
        &[1, 2],
        &[2, 1],
        &[1, 3],
        &[3, 1],
        &[2, 3],
        &[3, 2],
        &[1, 4],
        &[4, 1],
        &[1, 1, 2],
        &[2, 1, 1],
        &[1, 2, 3],
        &[3, 2, 1],
    ];
    for pat in patterns {
        if out.len() >= LIMIT {
            break;
        }
        let mut sizes = Vec::with_capacity(len);
        while sizes.iter().sum::<usize>() < len {
            sizes.extend_from_slice(pat);
        }
        let chunks = by_sizes(buf, &sizes);
        push_if_new(&mut out, &mut seen, chunks);
    }

    if out.len() >= LIMIT {
        return out;
    }

    // 5) Fibonacci-inspired growth to exercise jagged boundaries
    let mut fib = vec![1usize, 1, 2, 3, 5, 8, 13, 21, 34, 55];
    fib.retain(|&x| x <= len);
    if !fib.is_empty() {
        let chunks = by_sizes(buf, &fib);
        push_if_new(&mut out, &mut seen, chunks);
    }

    // Ensure we never exceed LIMIT
    if out.len() > LIMIT {
        out.truncate(LIMIT);
    }

    out
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
