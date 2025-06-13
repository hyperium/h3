/// Return type for parsing qpack stuff
#[derive(Debug)]
pub enum ParseProgressResult<T, E, R> {
    /// Error while parsing
    Error(E),
    /// Parsing is done
    Done(R),
    /// More data is needed to continue parsing
    MoreData(T),
}