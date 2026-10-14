//! Dev tool: `NAPSTER_SNAPSHOT_DIR=out napster --config cfg.json` logs in,
//! runs a search (`NAPSTER_SNAPSHOT_SEARCH`, default "mp3"), starts a couple
//! of downloads, then screenshots every tab and dialog to PNG and exits.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shot {
    Home,
    Chat,
    Library,
    Search,
    HotList,
    Transfer,
    Discover,
    Help,
    Prefs,
    Splash,
    Whois,
}

impl Shot {
    pub fn name(self) -> &'static str {
        match self {
            Shot::Home => "01-home",
            Shot::Chat => "02-chat",
            Shot::Library => "03-library",
            Shot::Search => "04-search",
            Shot::HotList => "05-hotlist",
            Shot::Transfer => "06-transfer",
            Shot::Discover => "07-discover",
            Shot::Help => "08-help",
            Shot::Prefs => "09-preferences",
            Shot::Splash => "10-logging-on",
            Shot::Whois => "11-whois",
        }
    }
}

pub const ORDER: [Shot; 11] = [
    Shot::Home,
    Shot::Chat,
    Shot::Library,
    Shot::Search,
    Shot::HotList,
    Shot::Transfer,
    Shot::Discover,
    Shot::Help,
    Shot::Prefs,
    Shot::Splash,
    Shot::Whois,
];

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    /// waiting to be online with a library
    Starting,
    /// search sent, waiting for results
    Searching,
    /// letting downloads and browse finish
    Settling,
    /// showing shot `n`, waiting for it to render
    Show(usize),
    /// screenshot requested for shot `n`
    Capture(usize),
    Done,
}

pub struct Snapshots {
    pub dir: PathBuf,
    pub search: String,
    /// `NAPSTER_SNAPSHOT_PLAY=1`: start the player so the Library shot shows
    /// a song playing. Off by default so screenshots never make noise.
    pub play: bool,
    pub phase: Phase,
    pub since: Instant,
}

impl Snapshots {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("NAPSTER_SNAPSHOT_DIR")?);
        let _ = std::fs::create_dir_all(&dir);
        Some(Self {
            dir,
            search: std::env::var("NAPSTER_SNAPSHOT_SEARCH").unwrap_or_else(|_| "mp3".into()),
            play: std::env::var("NAPSTER_SNAPSHOT_PLAY").is_ok_and(|v| v == "1"),
            phase: Phase::Starting,
            since: Instant::now(),
        })
    }

    pub fn go(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = Instant::now();
    }

    pub fn elapsed(&self) -> Duration {
        self.since.elapsed()
    }
}

/// Write an RGBA image as PNG (stored deflate blocks, no compression).
pub fn write_png(path: &Path, width: usize, height: usize, rgba: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(height * (width * 4 + 1));
    for row in rgba.chunks(width * 4) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    // zlib: header, stored blocks, adler32
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc() {
        assert_eq!(super::crc32(b"IEND"), 0xae42_6082);
    }
}
