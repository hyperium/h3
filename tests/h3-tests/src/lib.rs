use std::net::{Ipv6Addr, ToSocketAddrs};

use bytes::Bytes;
use futures::StreamExt;
use h3::quic;
use h3_quinn::{
    quinn::{
        Certificate, CertificateChain, ClientConfigBuilder, Endpoint, Incoming, PrivateKey,
        ServerConfigBuilder,
    },
    Connection,
};

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::FULL)
        .with_test_writer()
        .try_init();
}

#[derive(Clone)]
pub struct Pair {
    port: u16,
    cert_chain: CertificateChain,
    cert: Certificate,
    key: PrivateKey,
}

impl Pair {
    pub fn new() -> Self {
        let (cert_chain, cert, key) = build_certs();
        Self {
            port: 0,
            cert_chain,
            cert,
            key,
        }
    }

    pub fn server(&mut self) -> Server {
        let mut server_config = ServerConfigBuilder::default();
        server_config.protocols(&[b"h3"]);
        server_config
            .certificate(self.cert_chain.clone(), self.key.clone())
            .unwrap();

        let mut server_endpoint_builder = Endpoint::builder();
        server_endpoint_builder.listen(server_config.build());

        let (endpoint, incoming) = server_endpoint_builder
            .bind(&"[::]:0".parse().unwrap())
            .unwrap();

        self.port = endpoint.local_addr().unwrap().port();

        Server { incoming }
    }

    pub async fn client(&self) -> impl quic::Connection<Bytes> {
        let addr = (Ipv6Addr::LOCALHOST, self.port)
            .to_socket_addrs()
            .unwrap()
            .next()
            .unwrap();

        let mut client_config = ClientConfigBuilder::default();
        client_config.protocols(&[b"h3"]);
        client_config
            .add_certificate_authority(self.cert.clone())
            .unwrap();

        let mut client_endpoint_builder = Endpoint::builder();
        client_endpoint_builder.default_client_config(client_config.build());
        let (client_endpoint, _) = client_endpoint_builder
            .bind(&"[::]:0".parse().unwrap())
            .unwrap();

        Connection::new(
            client_endpoint
                .connect(&addr, "localhost")
                .unwrap()
                .await
                .unwrap(),
        )
    }
}

pub struct Server {
    incoming: Incoming,
}

impl Server {
    pub async fn next(&mut self) -> impl quic::Connection<Bytes> {
        Connection::new(self.incoming.next().await.unwrap().await.unwrap())
    }
}

pub fn build_certs() -> (CertificateChain, Certificate, PrivateKey) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = PrivateKey::from_der(&cert.serialize_private_key_der()).unwrap();
    let cert = Certificate::from_der(&cert.serialize_der().unwrap()).unwrap();
    (CertificateChain::from_certs(vec![cert.clone()]), cert, key)
}
