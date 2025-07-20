use bytes::Buf;

/// Return type for parsing qpack stuff
#[derive(Debug)]
pub enum ParseProgressResult<T: Sized, E, R> {
    /// Error while parsing
    Error(E),
    /// Parsing is done
    Done(R),
    /// More data is needed to continue parsing
    MoreData(T),
}

/// Trait for stateful parsers
pub trait StatefulParser<B, E, R>
where
    B: Buf,
    Self: Sized,
{
    /// Parse the next chunk of data
    fn parse_progress(self, reader: &mut B) -> ParseProgressResult<Self, E, R>;
}

