//! Napster 2.0 wire protocol.
//!
//! Every client <-> server message is framed as
//!
//! ```text
//! +----------------+----------------+---------------------+
//! | length: u16 LE | type:   u16 LE | data: `length` bytes |
//! +----------------+----------------+---------------------+
//! ```
//!
//! where `data` is plain text whose fields are separated by single spaces and
//! where file names and client strings are wrapped in double quotes (with no
//! escaping). The message type numbers in [`msg`] were recovered from the
//! dispatcher in `napster.exe` v2.0 BETA 10 (see `docs/PROTOCOL.md`).
//!
//! Peer to peer transfers use a separate, unframed protocol described in
//! [`peer`].

pub mod args;
pub mod codec;
pub mod ip;
pub mod msg;
pub mod peer;
pub mod search;
pub mod share;

pub use args::{quote, Args};
pub use codec::{Message, NapCodec, MAX_PAYLOAD};
pub use search::{Compare, SearchQuery};
pub use share::{FileInfo, SearchResult};

/// Default TCP port of the metaserver (redirector). The original client
/// connects here first (`server.napster.com:8875`) to be handed a server.
pub const METASERVER_PORT: u16 = 8875;
/// Default TCP port for the main server.
pub const SERVER_PORT: u16 = 8888;
/// Default data port clients listen on for peer transfers.
pub const DATA_PORT: u16 = 6699;
/// Build number the 2.0 BETA 10 client sends after its link speed on login.
pub const CLIENT_BUILD: u32 = 5151;

/// Link speeds as used by the login (2) message and in search results.
pub const LINK_SPEEDS: [&str; 11] = [
    "Unknown",
    "14.4 Kbps",
    "28.8 Kbps",
    "33.6 Kbps",
    "56K",
    "ISDN-64K",
    "ISDN-128K",
    "Cable",
    "DSL",
    "T1",
    "T3+",
];

pub fn link_speed_name(speed: u32) -> &'static str {
    LINK_SPEEDS.get(speed as usize).copied().unwrap_or("Unknown")
}

/// Characters allowed in a nickname by the Napster, Inc. servers.
pub fn valid_nick(nick: &str) -> bool {
    !nick.is_empty()
        && nick.len() <= 30
        && nick
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_[]{}-@^!$|`".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nick_validation() {
        assert!(valid_nick("lefty"));
        assert!(valid_nick("[foo]-bar_99"));
        assert!(!valid_nick("has space"));
        assert!(!valid_nick(""));
        assert!(!valid_nick("quote\"d"));
    }
}
