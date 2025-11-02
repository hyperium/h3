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

/// implement PartialEq for ParseProgressResult
#[cfg(test)]
impl<T, E: PartialEq, R: PartialEq> PartialEq for ParseProgressResult<T, E, R> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ParseProgressResult::Error(e1), ParseProgressResult::Error(e2)) => e1 == e2,
            (ParseProgressResult::Done(r1), ParseProgressResult::Done(r2)) => r1 == r2,
            (ParseProgressResult::MoreData(_), ParseProgressResult::MoreData(_)) => false, // T is not comparable. This is for tests so we can ignore it.
            _ => false,
        }
    }
}

/// Trait for stateful parsers
pub trait StatefulParser<E, R>
where
    Self: Sized,
{
    /// Parse the next chunk of data
    fn parse_progress<B: Buf>(self, reader: &mut B) -> ParseProgressResult<Self, E, R>;
}
