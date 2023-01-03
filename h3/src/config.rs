//! HTTP/3 endpoint config

/// Default max header size
pub const DEFAULT_MAX_FIELD_SECTION_SIZE: u64 = (1 << 62) - 1;

/// Client config builder
#[derive(Debug, Default)]
pub struct ClientConfig {
    pub(crate) conn: ConnectionConfig,
}

/// Server config builder
#[derive(Debug, Default)]
pub struct ServerConfig {
    pub(crate) conn: ConnectionConfig,
}

/// Configuration for [`ConnectionInner`]
#[derive(Clone, Copy, Debug)]
pub struct ConnectionConfig {
    pub(crate) enable_grease: bool,
    pub(crate) enable_webtransport: bool,
    pub(crate) max_field_section_size: u64,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            enable_grease: true,
            enable_webtransport: false,
            max_field_section_size: DEFAULT_MAX_FIELD_SECTION_SIZE,
        }
    }
}

macro_rules! impl_builder {
    ($name:ident) => {
        impl $name {
            /// Start to build config
            pub fn new() -> Self {
                Self::default()
            }

            /// Disable grease in SETTINGS
            pub fn disable_grease(mut self) -> Self {
                self.conn.enable_grease = false;
                self
            }

            /// Enable WebTransport (not supported yet)
            pub fn enable_webtransport(mut self) -> Self {
                self.conn.enable_webtransport = true;
                self
            }

            /// Set maximum header size the endpoint is willing to accept
            ///
            /// See [header size constraints] section of the specification for details.
            ///
            /// [header size constraints]: https://www.rfc-editor.org/rfc/rfc9114.html#name-header-size-constraints
            pub fn max_field_section_size(mut self, val: u64) -> Self {
                self.conn.max_field_section_size = val;
                self
            }
        }
    };
}

impl_builder!(ClientConfig);
impl_builder!(ServerConfig);
