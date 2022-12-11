//! HTTP/3 endpoint config

/// Default max header size
pub const DEFAULT_MAX_FIELD_SECTION_SIZE: u64 = (1 << 62) - 1;

macro_rules! config_struct {
    (
        $(#[$doc:meta])*
        $name:ident { $($fn:ident: $ft:ty),*$(,)? }
    ) => {
        $(#[$doc])*
        #[derive(Debug)]
        pub struct $name {
            // common fields
            pub(crate) enable_grease: bool,
            pub(crate) enable_webtransport: bool,
            pub(crate) max_field_section_size: u64,

            // fields specific to the config
            $(pub(crate) $fn: $ft),*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    enable_grease: true,
                    enable_webtransport: false,
                    max_field_section_size: DEFAULT_MAX_FIELD_SECTION_SIZE,
                }
            }
        }

        impl $name {
            /// Start to build config
            pub fn new() -> Self {
                Self::default()
            }

            /// Enable WebTransport
            pub fn enable_webtransport(mut self) -> Self {
                self.enable_webtransport = true;
                self
            }

            /// Disable grease in SETTINGS
            pub fn disable_grease(mut self) -> Self {
                self.enable_grease = false;
                self
            }

            /// Set maximum header size the endpoint is willing to accept
            ///
            /// See [header size constraints] section of the specification for details.
            ///
            /// [header size constraints]: https://www.rfc-editor.org/rfc/rfc9114.html#name-header-size-constraints
            pub fn max_field_section_size(mut self, val: u64) -> Self {
                self.max_field_section_size = val;
                self
            }
        }
    }
}

config_struct!(
    /// Client endpoint config builder
    ClientConfig {}
);

config_struct!(
    /// Server endpoint config builder
    ServerConfig {}
);
