//! Traits which define the user API for datagrams.
//! These traits are implemented for the client and server types in the `h3` crate.

use bytes::Buf;
use h3::{
    quic::{self, StreamId},
    Error,
};

use crate::server::ReadDatagram;

pub trait HandleDatagramsExt<C, B>
where
    B: Buf,
    C: quic::Connection<B>,
{
    /// Sends a datagram
    fn send_datagram(&mut self, stream_id: StreamId, data: B) -> Result<(), Error>;
    /// Reads an incoming datagram
    fn read_datagram(&mut self) -> ReadDatagram<C, B>;
}
