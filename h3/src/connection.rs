use std::marker::PhantomData;

use bytes::Bytes;
use futures::future;

use crate::{quic, Error};

pub struct ConnectionInner<C: quic::Connection<Bytes>> {
    pub(super) quic: C,
    max_field_section_size: u64,
    control_send: C::SendStream,
}

impl<C> ConnectionInner<C>
where
    C: quic::Connection<Bytes>,
{
    pub async fn new(mut quic: C, max_field_section_size: u64) -> Result<Self, Error> {
        let control_send = future::poll_fn(|mut cx| quic.poll_open_send_stream(&mut cx))
            .await
            .map_err(|e| Error::Io(e.into()))?;

        Ok(Self {
            quic,
            control_send,
            max_field_section_size,
        })
    }
}

pub struct Builder<T> {
    pub(super) max_field_section_size: u64,
    phtantom: PhantomData<T>,
}

impl<T> Builder<T> {
    pub(super) fn new() -> Self {
        Builder {
            max_field_section_size: 0, // Unlimited
            phtantom: PhantomData,
        }
    }

    pub fn max_field_section_size(&mut self, value: u64) -> &mut Self {
        self.max_field_section_size = value;
        self
    }
}
