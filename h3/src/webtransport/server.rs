//! Provides the server side WebTransport session
use bytes::Buf;

use crate::{quic, server};

/// WebTransport session driver.
///
/// Maintains the session using the underlying HTTP/3 connection.
///
/// Similar to [`crate::Connection`] it is generic over the QUIC implementation and Buffer.
pub struct WebTransportSession<C, B>
where
    C: quic::Connection<B>,
    B: Buf,
{
    conn: server::Connection<C, B>,
}
