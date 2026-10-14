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
pub mod tls;
mod transfer;
pub mod types;
pub mod update;

pub use config::{ClientConfig, PERIOD_RATES};
pub use engine::Client;
pub use types::*;
pub use update::UpdateInfo;

/// File types the client will download. Anything else (say,
/// `song.mp3.exe`) is refused: fake songs carrying malware were a classic
/// Napster-era trick.
pub const AUDIO_EXTENSIONS: [&str; 9] = ["mp3", "mp2", "mp1", "wma", "ogg", "flac", "wav", "m4a", "aac"];

/// Is `name` an audio file by its (last) extension?
pub fn is_audio_file(name: &str) -> bool {
    let base = napster_proto::share::basename(name);
    base.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && AUDIO_EXTENSIONS.iter().any(|a| a.eq_ignore_ascii_case(ext.trim())))
}

/// Client string sent in the login message.
pub const CLIENT_INFO: &str = concat!("napster-rs ", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
mod tests {
    #[test]
    fn audio_detection() {
        assert!(super::is_audio_file("C:\\MP3\\Song.mp3"));
        assert!(super::is_audio_file("/music/a.FLAC"));
        assert!(!super::is_audio_file("Song.mp3.exe"));
        assert!(!super::is_audio_file("Song.mp3.scr"));
        assert!(!super::is_audio_file(".mp3"));
        assert!(!super::is_audio_file("noext"));
    }

    #[test]
    fn local_addresses() {
        use crate::engine::is_local_ip;
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.5", "172.16.9.9", "169.254.1.1", "100.64.0.1", "::1", "fd00::1", "fe80::1"] {
            assert!(is_local_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "205.188.1.1", "2001:4860::8888"] {
            assert!(!is_local_ip(ip.parse().unwrap()), "{ip}");
        }
    }
}

