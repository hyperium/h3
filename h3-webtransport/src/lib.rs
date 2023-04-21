//! Provides the client and server support for WebTransport sessions.
//!
//! # Relevant Links
//! WebTransport: https://www.w3.org/TR/webtransport/#biblio-web-transport-http3
//! WebTransport over HTTP/3: https://datatracker.ietf.org/doc/html/draft-ietf-webtrans-http3/

use std::convert::TryFrom;

use h3::proto::{
    coding::{Decode, Encode},
    stream::{InvalidStreamId, StreamId},
    varint::VarInt,
};
pub mod server;
