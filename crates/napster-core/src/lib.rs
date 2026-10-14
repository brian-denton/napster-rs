//! Napster client engine.
//!
//! [`Client::start`] spawns the engine on the current tokio runtime and
//! returns a handle for sending [`Command`]s plus a receiver of [`Event`]s.
//! The GUI and CLI front ends are thin layers over this.

pub mod config;
mod engine;
pub mod library;
pub mod mp3;
mod throttle;
mod transfer;
pub mod types;

pub use config::{ClientConfig, PERIOD_RATES};
pub use engine::Client;
pub use types::*;

/// Client string sent in the login message.
pub const CLIENT_INFO: &str = concat!("napster-rs ", env!("CARGO_PKG_VERSION"));
