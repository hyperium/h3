//! HTTP/3 connection parameters

/// HTTP/3 connection parameters builder
pub struct Params {
    pub(crate) enable_webtransport: bool,
    pub(crate) grease: bool,
    pub(crate) max_field_section_size: u64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            enable_webtransport: false,
            grease: true,
            max_field_section_size: Self::DEFAULT_MAX_FIELD_SECTION_SIZE,
        }
    }
}

impl Params {
    /// Default max header size
    pub const DEFAULT_MAX_FIELD_SECTION_SIZE: u64 = (1 << 62) - 1;

    /// Set whether WebTransport is supported
    pub fn enable_webtransport(mut self, val: bool) -> Self {
        self.enable_webtransport = val;
        self
    }

    /// Set wether to send GREASE
    pub fn grease(mut self, val: bool) -> Self {
        self.grease = val;
        self
    }

    /// Set the maximum header size the endpoint is willing to accept
    ///
    /// See [header size constraints] section of the specification for details.
    ///
    /// [header size constraints]: https://www.rfc-editor.org/rfc/rfc9114.html#name-header-size-constraints
    pub fn max_field_section_size(mut self, val: u64) -> Self {
        self.max_field_section_size = val;
        self
    }
}
