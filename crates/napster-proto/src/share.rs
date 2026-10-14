//! Shared file descriptions as they appear in 100 (share), 201 (search
//! result), 212 (browse result) and direct-browse lists.

use std::net::Ipv4Addr;

use crate::args::{quote, Args};
use crate::ip;

/// A file someone is sharing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileInfo {
    /// Full path as the sharing client knows it, e.g. `C:\MP3\song.mp3`.
    pub filename: String,
    /// MD5 of the first 299,008 bytes of audio, or `WMA-FILE`.
    pub md5: String,
    pub size: u64,
    /// kbps
    pub bitrate: u32,
    /// Hz
    pub frequency: u32,
    pub seconds: u32,
}

impl FileInfo {
    /// `"<file>" <md5> <size> <bitrate> <freq> <secs>` (message 100 and the
    /// direct browse list format).
    pub fn to_share(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            quote(&self.filename),
            self.md5,
            self.size,
            self.bitrate,
            self.frequency,
            self.seconds
        )
    }

    pub fn parse_share(s: &str) -> Option<Self> {
        Self::from_args(&mut Args::new(s))
    }

    pub fn from_args(a: &mut Args<'_>) -> Option<Self> {
        Some(Self {
            filename: a.str()?.to_owned(),
            md5: a.str()?.to_owned(),
            size: a.num()?,
            bitrate: a.num()?,
            frequency: a.num()?,
            seconds: a.num()?,
        })
    }

    /// Final path component regardless of whether the sharer used `\` or `/`.
    pub fn basename(&self) -> &str {
        basename(&self.filename)
    }
}

pub fn basename(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// A search hit (201) or browse entry (212, which lacks ip/speed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub file: FileInfo,
    pub nick: String,
    pub ip: Ipv4Addr,
    pub link_speed: u32,
}

impl SearchResult {
    /// `"<file>" <md5> <size> <bitrate> <freq> <secs> <nick> <ip> <speed>`
    pub fn to_payload(&self) -> String {
        format!(
            "{} {} {} {}",
            self.file.to_share(),
            self.nick,
            ip::encode(self.ip),
            self.link_speed
        )
    }

    pub fn parse(s: &str) -> Option<Self> {
        let mut a = Args::new(s);
        let file = FileInfo::from_args(&mut a)?;
        let nick = a.str()?.to_owned();
        let ip = ip::parse(a.str()?)?;
        let link_speed = a.num().unwrap_or(0);
        Some(Self { file, nick, ip, link_speed })
    }

    /// Parse a 212 browse entry: `<nick> "<file>" <md5> <size> <bitrate> <freq> <secs>`.
    pub fn parse_browse(s: &str) -> Option<Self> {
        let mut a = Args::new(s);
        let nick = a.str()?.to_owned();
        let file = FileInfo::from_args(&mut a)?;
        Some(Self { file, nick, ip: Ipv4Addr::UNSPECIFIED, link_speed: 0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_result_roundtrip() {
        let s = "\"random band - random song.mp3\" 7d733c1e7419674744768db71bff8bcd 2558199 128 44100 159 lefty 3437166285 4";
        let r = SearchResult::parse(s).unwrap();
        assert_eq!(r.nick, "lefty");
        assert_eq!(r.file.size, 2558199);
        assert_eq!(r.ip, Ipv4Addr::new(205, 254, 222, 204));
        assert_eq!(r.to_payload(), s);
    }

    #[test]
    fn browse_entry() {
        let r = SearchResult::parse_browse(
            "foouser \"C:\\MP3\\generic band - generic song.mp3\" b92870e0d41bc8e698cf2f0a1ddfeac7 443332 128 44100 60",
        )
        .unwrap();
        assert_eq!(r.nick, "foouser");
        assert_eq!(r.file.basename(), "generic band - generic song.mp3");
    }
}
