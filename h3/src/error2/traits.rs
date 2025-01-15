//! Defines error traits

use crate::{config::Config, shared_state::ConnectionState2};

use super::{codes::NewCode, internal_error::InternalRequestStreamError, ConnectionError};

/// This trait is implemented for all types which can close the connection
pub(crate) trait CloseConnection: ConnectionState2 {
    /// Close the connection
    fn handle_connection_error(
        &mut self,
        internal_error: InternalRequestStreamError,
    ) -> ConnectionError {
        //self.maybe_conn_error(error)
        todo!()
    }

    fn close_connection<T: AsRef<str>>(code: &NewCode, reason: T) -> ();
}

pub(crate) trait CloseStream: CloseConnection {
    fn handle_stream_error(
        &mut self,
        internal_error: InternalRequestStreamError,
        config: &Config,
    ) -> ConnectionError {
        todo!()
    }

    fn close_stream() -> ();
}
