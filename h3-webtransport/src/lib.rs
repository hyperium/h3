//! Provides the client and server support for WebTransport sessions.
//!
//! # Relevant Links
//! WebTransport: <https://www.w3.org/TR/webtransport/#biblio-web-transport-http3>
//! WebTransport over HTTP/3: <https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/>
#![deny(missing_docs)]

/// Server side WebTransport session support
pub mod server;
/// Webtransport stream types
pub mod stream;

pub use h3::webtransport::SessionId;
