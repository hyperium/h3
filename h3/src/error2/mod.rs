//! This module contains the error handling logic and types for the h3 crate.
//!
//! # Error handling
//! There are two ways an error can occur within h3
//!
//! 1. the h3 instance itself can encounter an error for example because the peer sent an unexpected frame or an incomplete frame
//! 2. the peer closes the connection with an error code
//!
//! ## Error types
//! Users of the h3 crate
//!

mod codes;
mod traits;

pub(crate) mod internal_error;

mod error;

pub use error::{ConnectionError, LocalError, StreamError};
