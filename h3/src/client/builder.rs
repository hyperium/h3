//! HTTP/3 client builder

use std::{
    marker::PhantomData,
    sync::{atomic::AtomicUsize, Arc},
    task::Poll,
};

use bytes::{Buf, Bytes};
use futures_util::future;

use crate::{
    config::Config,
    connection::{ConnectionInner, SharedStateRef},
    error::Error,
    quic::{self},
};

use super::connection::{Connection, SendRequest};

/// Start building a new HTTP/3 client
pub fn builder() -> Builder {
    Builder::new()
}

/// Create a new HTTP/3 client with default settings
pub async fn new<C, O>(conn: C) -> Result<(Connection<C, Bytes>, SendRequest<O, Bytes>), Error>
where
    C: quic::Connection<Bytes, OpenStreams = O>,
    O: quic::OpenStreams<Bytes>,
{
    //= https://www.rfc-editor.org/rfc/rfc9114#section-3.3
    //= type=implication
    //# Clients SHOULD NOT open more than one HTTP/3 connection to a given IP
    //# address and UDP port, where the IP address and port might be derived
    //# from a URI, a selected alternative service ([ALTSVC]), a configured
    //# proxy, or name resolution of any of these.
    Builder::new().build(conn).await
}

/// HTTP/3 client builder
///
/// Set the configuration for a new client.
///
/// # Examples
/// ```rust
/// # use h3::quic;
/// # async fn doc<C, O, B>(quic: C)
/// # where
/// #   C: quic::Connection<B, OpenStreams = O>,
/// #   O: quic::OpenStreams<B>,
/// #   B: bytes::Buf,
/// # {
/// let h3_conn = h3::client::builder()
///     .max_field_section_size(8192)
///     .build(quic)
///     .await
///     .expect("Failed to build connection");
/// # }
/// ```
pub struct Builder {
    config: Config,
}

impl Builder {
    pub(super) fn new() -> Self {
        Builder {
            config: Default::default(),
        }
    }

    #[cfg(test)]
    pub fn send_settings(&mut self, value: bool) -> &mut Self {
        self.config.send_settings = value;
        self
    }

    /// Set the maximum header size this client is willing to accept
    ///
    /// See [header size constraints] section of the specification for details.
    ///
    /// [header size constraints]: https://www.rfc-editor.org/rfc/rfc9114.html#name-header-size-constraints
    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.config.settings.max_field_section_size = value;
        self
    }

    /// Just like in HTTP/2, HTTP/3 also uses the concept of "grease"
    /// to prevent potential interoperability issues in the future.
    /// In HTTP/3, the concept of grease is used to ensure that the protocol can evolve
    /// and accommodate future changes without breaking existing implementations.
    pub fn send_grease(&mut self, enabled: bool) -> &mut Self {
        self.config.send_grease = enabled;
        self
    }

    /// Create a new HTTP/3 client from a `quic` connection
    pub async fn build<C, O, B>(
        &mut self,
        quic: C,
    ) -> Result<(Connection<C, B>, SendRequest<O, B>), Error>
    where
        C: quic::Connection<B, OpenStreams = O>,
        O: quic::OpenStreams<B>,
        B: Buf,
    {
        let open = quic.opener();
        let conn_state = SharedStateRef::default();

        let conn_waker = Some(future::poll_fn(|cx| Poll::Ready(cx.waker().clone())).await);

        Ok((
            Connection {
                inner: ConnectionInner::new(quic, conn_state.clone(), self.config).await?,
                sent_closing: None,
                recv_closing: None,
            },
            SendRequest {
                open,
                conn_state,
                conn_waker,
                max_field_section_size: self.config.settings.max_field_section_size,
                sender_count: Arc::new(AtomicUsize::new(1)),
                send_grease_frame: self.config.send_grease,
                _buf: PhantomData,
            },
        ))
    }
}
