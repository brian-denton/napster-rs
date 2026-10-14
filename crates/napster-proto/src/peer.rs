//! Peer to peer (client <-> client) transfer protocol.
//!
//! This is unframed. Recovered from `DownloadFile()` in napster.exe at
//! 0x410fb0 and confirmed against the community spec:
//!
//! ```text
//! normal download (downloader connects to uploader's data port)
//!   uploader   -> "1"
//!   downloader -> "GET"
//!   downloader -> <mynick> "<filename>" <offset>
//!   uploader   -> <total size as ascii digits> | "FILE NOT SHARED" | "INVALID REQUEST" | ...
//!   uploader   -> file bytes from <offset>
//!
//! firewalled upload (uploader connects to downloader's data port)
//!   downloader -> "1"
//!   uploader   -> "SEND"
//!   uploader   -> <mynick> "<filename>" <size>
//!   downloader -> <offset as ascii digits> | "INVALID REQUEST"
//!   uploader   -> file bytes from <offset>
//!
//! direct browse (640/641)
//!   browsee    -> "1"
//!   browser    -> "GETLIST"
//!   browsee    -> <nick> LF ( "<file>" <md5> <size> <bitrate> <freq> <secs> LF )* LF, close
//!
//! firewalled direct browse (browsee connects)
//!   browser    -> "1"
//!   browsee    -> "SENDLIST <nick>" LF, then the list as above
//! ```

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use crate::args::{quote, Args};
use crate::codec::decode_text;
use crate::share::FileInfo;

pub const ACK: u8 = b'1';
pub const GET: &[u8] = b"GET";
pub const SEND: &[u8] = b"SEND";
pub const GETLIST: &[u8] = b"GETLIST";
pub const SENDLIST: &[u8] = b"SENDLIST";

pub const FILE_NOT_SHARED: &str = "FILE NOT SHARED";
pub const FILE_NOT_FOUND: &str = "FILE NOT FOUND";
pub const INVALID_REQUEST: &str = "INVALID REQUEST";

/// How long to wait for the peer before giving up on the handshake.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// After a complete-looking request, wait this long for trailing digits.
const SETTLE: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerRequest {
    /// Remote wants `filename` from us starting at `offset`.
    Get { nick: String, filename: String, offset: u64 },
    /// Remote (firewalled) is pushing `filename` of `size` bytes to us.
    Send { nick: String, filename: String, size: u64 },
    /// Remote wants our shared file list.
    GetList,
    /// Remote (firewalled) is pushing their file list; `leftover` holds any
    /// list bytes that arrived with the header.
    SendList { nick: String, leftover: Vec<u8> },
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "peer handshake timed out")
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

async fn read_some<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>, wait: Duration) -> io::Result<usize> {
    let mut chunk = [0u8; 4096];
    let n = timeout(wait, s.read(&mut chunk)).await.map_err(|_| timed_out())??;
    buf.extend_from_slice(&chunk[..n]);
    Ok(n)
}

/// Read until `buf` holds `<nick> "<file>" <digits>`, then linger briefly
/// so a number split across packets isn't truncated.
async fn read_request_line<S: AsyncRead + Unpin>(s: &mut S, buf: &mut Vec<u8>) -> io::Result<(String, String, u64)> {
    loop {
        if let Some(parsed) = parse_request_line(buf) {
            // Collect any trailing digits still in flight.
            match timeout(SETTLE, async {
                let mut chunk = [0u8; 64];
                s.read(&mut chunk).await.map(|n| chunk[..n].to_vec())
            })
            .await
            {
                Ok(Ok(more)) if !more.is_empty() => {
                    buf.extend_from_slice(&more);
                    continue;
                }
                _ => return Ok(parsed),
            }
        }
        if buf.len() > 8192 {
            return Err(invalid("peer request too long"));
        }
        if read_some(s, buf, HANDSHAKE_TIMEOUT).await? == 0 {
            return parse_request_line(buf).ok_or_else(|| invalid(INVALID_REQUEST));
        }
    }
}

fn parse_request_line(buf: &[u8]) -> Option<(String, String, u64)> {
    let text = decode_text(buf);
    let mut a = Args::new(text.trim_start());
    let nick = a.str()?.to_owned();
    // filename must be quoted
    let after_nick = text.trim_start()[nick.len()..].trim_start();
    if !after_nick.starts_with('"') || after_nick[1..].find('"').is_none() {
        return None;
    }
    let filename = a.str()?.to_owned();
    let num = a.str()?;
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((nick, filename, num.parse().ok()?))
}

/// Listener side: send the `1` greeting and read what the peer wants.
pub async fn accept<S>(s: &mut S) -> io::Result<PeerRequest>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    s.write_all(&[ACK]).await?;
    s.flush().await?;
    let mut buf = Vec::new();
    loop {
        if buf.starts_with(GETLIST) {
            return Ok(PeerRequest::GetList);
        }
        if buf.starts_with(SENDLIST) {
            if let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let nick = decode_text(&buf[SENDLIST.len()..nl]).trim().to_owned();
                return Ok(PeerRequest::SendList { nick, leftover: buf[nl + 1..].to_vec() });
            }
        } else if buf.starts_with(SEND) && !SENDLIST.starts_with(&buf) {
            let mut rest = buf[SEND.len()..].to_vec();
            let (nick, filename, size) = read_request_line(s, &mut rest).await?;
            return Ok(PeerRequest::Send { nick, filename, size });
        } else if buf.starts_with(GET) && !GETLIST.starts_with(&buf) {
            let mut rest = buf[GET.len()..].to_vec();
            let (nick, filename, offset) = read_request_line(s, &mut rest).await?;
            return Ok(PeerRequest::Get { nick, filename, offset });
        } else if buf.starts_with(GET) {
            // "GET" or a prefix of "GETLIST": the b10 client sends "GET" as
            // its own packet, so give "LIST" a moment, then assume GET.
            match timeout(SETTLE, read_some(s, &mut buf, HANDSHAKE_TIMEOUT)).await {
                Ok(Ok(0)) => return Err(invalid(INVALID_REQUEST)),
                Ok(r) => {
                    r?;
                    continue;
                }
                Err(_) => {
                    let mut rest = buf[GET.len()..].to_vec();
                    let (nick, filename, offset) = read_request_line(s, &mut rest).await?;
                    return Ok(PeerRequest::Get { nick, filename, offset });
                }
            }
        } else if buf.len() >= SENDLIST.len()
            || (!buf.is_empty() && !GETLIST.starts_with(&buf) && !SENDLIST.starts_with(&buf))
        {
            return Err(invalid(INVALID_REQUEST));
        }
        if read_some(s, &mut buf, HANDSHAKE_TIMEOUT).await? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed during handshake"));
        }
    }
}

/// Connecting side: wait for the `1` greeting.
pub async fn expect_ack<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<()> {
    let mut b = [0u8; 1];
    timeout(HANDSHAKE_TIMEOUT, s.read_exact(&mut b)).await.map_err(|_| timed_out())??;
    if b[0] != ACK {
        return Err(invalid(format!("expected '1' greeting, got 0x{:02x}", b[0])));
    }
    Ok(())
}

/// `<nick> "<filename>" <n>`
pub fn request_line(nick: &str, filename: &str, n: u64) -> String {
    format!("{} {} {}", nick, quote(filename), n)
}

/// Downloader: after the greeting, request `filename` from `offset`.
pub async fn send_get<S: AsyncWrite + Unpin>(s: &mut S, nick: &str, filename: &str, offset: u64) -> io::Result<()> {
    s.write_all(GET).await?;
    s.flush().await?;
    s.write_all(request_line(nick, filename, offset).as_bytes()).await?;
    s.flush().await
}

/// Firewalled uploader: after the greeting, announce the push.
pub async fn send_push<S: AsyncWrite + Unpin>(s: &mut S, nick: &str, filename: &str, size: u64) -> io::Result<()> {
    s.write_all(SEND).await?;
    s.flush().await?;
    s.write_all(request_line(nick, filename, size).as_bytes()).await?;
    s.flush().await
}

/// Result of reading the uploader's reply to GET.
#[derive(Debug)]
pub enum SizeReply {
    /// File size and any file bytes that arrived in the same read.
    Size(u64, Vec<u8>),
    /// The uploader refused (e.g. `FILE NOT SHARED`).
    Error(String),
}

/// Downloader: read the ascii file size that precedes the data.
///
/// The size isn't delimited, so when the expected size is known (it is, from
/// the search result) we use it to split digits from data; otherwise we stop
/// at the first non-digit like the original client.
pub async fn read_size<S: AsyncRead + Unpin>(s: &mut S, expected: Option<u64>) -> io::Result<SizeReply> {
    let mut buf = Vec::new();
    let expected_str = expected.map(|n| n.to_string());
    loop {
        let n = read_some(s, &mut buf, HANDSHAKE_TIMEOUT).await?;
        let digits = buf.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 && !buf.is_empty() {
            // error text; collect whatever else arrives promptly
            let _ = timeout(SETTLE, read_some(s, &mut buf, SETTLE)).await;
            return Ok(SizeReply::Error(decode_text(&buf).trim().to_owned()));
        }
        if buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed before sending size"));
        }
        let parsed = std::str::from_utf8(&buf[..digits]).unwrap().parse::<u64>().ok();
        let expected_prefix = expected_str.as_ref().filter(|es| buf.starts_with(es.as_bytes()));
        if digits < buf.len() {
            // a non-digit marks the start of the data, unless the data itself
            // starts with digits (detectable when we know the size)
            if parsed != expected {
                if let Some(es) = expected_prefix {
                    return Ok(SizeReply::Size(expected.unwrap(), buf[es.len()..].to_vec()));
                }
            }
            let size = parsed.ok_or_else(|| invalid("bad size"))?;
            return Ok(SizeReply::Size(size, buf[digits..].to_vec()));
        }
        // only digits so far
        if let Some(es) = expected_prefix {
            return Ok(SizeReply::Size(expected.unwrap(), buf[es.len()..].to_vec()));
        }
        if n == 0 {
            let size = parsed.ok_or_else(|| invalid("bad size"))?;
            return Ok(SizeReply::Size(size, Vec::new()));
        }
    }
}

/// Firewalled uploader: read the offset the downloader wants.
pub async fn read_offset<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<Result<u64, String>> {
    let mut buf = Vec::new();
    read_some(s, &mut buf, HANDSHAKE_TIMEOUT).await?;
    while let Ok(Ok(n)) = timeout(SETTLE, read_some(s, &mut buf, SETTLE)).await {
        if n == 0 {
            break;
        }
    }
    let text = decode_text(&buf);
    let t = text.trim();
    Ok(t.parse::<u64>().map_err(|_| t.to_owned()))
}

/// Serialize a direct-browse list.
pub fn encode_list(nick: &str, files: &[FileInfo]) -> Vec<u8> {
    let mut out = format!("{nick}\n");
    for f in files {
        out.push_str(&f.to_share());
        out.push('\n');
    }
    out.push('\n');
    out.into_bytes()
}

/// Read a direct-browse list to EOF. `prefix` is data already read; set
/// `has_nick` when the nick line is still to come (GETLIST replies).
pub async fn read_list<S: AsyncRead + Unpin>(
    s: &mut S,
    prefix: Vec<u8>,
    has_nick: bool,
) -> io::Result<(Option<String>, Vec<FileInfo>)> {
    let mut buf = prefix;
    timeout(Duration::from_secs(120), s.read_to_end(&mut buf)).await.map_err(|_| timed_out())??;
    let text = decode_text(&buf);
    let mut lines = text.split('\n').map(|l| l.trim_end_matches('\r'));
    let nick = if has_nick { lines.next().map(|l| l.trim().to_owned()) } else { None };
    let files = lines.take_while(|l| !l.is_empty()).filter_map(FileInfo::parse_share).collect();
    Ok((nick, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn get_handshake_split_packets() {
        let (mut a, mut b) = duplex(1 << 16);
        let server = tokio::spawn(async move { accept(&mut a).await.map(|r| (r, a)) });
        expect_ack(&mut b).await.unwrap();
        send_get(&mut b, "lefty", "C:\\MP3\\song.mp3", 1234).await.unwrap();
        let (req, mut a) = server.await.unwrap().unwrap();
        assert_eq!(
            req,
            PeerRequest::Get { nick: "lefty".into(), filename: "C:\\MP3\\song.mp3".into(), offset: 1234 }
        );
        a.write_all(b"5000\xff\xfbdata").await.unwrap();
        match read_size(&mut b, Some(5000)).await.unwrap() {
            SizeReply::Size(n, rest) => {
                assert_eq!(n, 5000);
                assert_eq!(rest, b"\xff\xfbdata");
            }
            e => panic!("{e:?}"),
        }
    }

    #[tokio::test]
    async fn size_followed_by_digit_data() {
        // file data starting with a digit is ambiguous without the expected size
        let (mut a, mut b) = duplex(1024);
        a.write_all(b"1234567").await.unwrap();
        match read_size(&mut b, Some(123)).await.unwrap() {
            SizeReply::Size(n, rest) => {
                assert_eq!(n, 123);
                assert_eq!(rest, b"4567");
            }
            e => panic!("{e:?}"),
        }
    }

    #[tokio::test]
    async fn getlist_and_sendlist() {
        let (mut a, mut b) = duplex(1024);
        let server = tokio::spawn(async move { accept(&mut a).await });
        expect_ack(&mut b).await.unwrap();
        b.write_all(GETLIST).await.unwrap();
        assert_eq!(server.await.unwrap().unwrap(), PeerRequest::GetList);

        let (mut a, mut b) = duplex(1024);
        let server = tokio::spawn(async move { accept(&mut a).await });
        expect_ack(&mut b).await.unwrap();
        b.write_all(b"SENDLIST bob\n\"x.mp3\" abc 1 128 44100 1\n").await.unwrap();
        match server.await.unwrap().unwrap() {
            PeerRequest::SendList { nick, leftover } => {
                assert_eq!(nick, "bob");
                assert!(leftover.starts_with(b"\"x.mp3\""));
            }
            r => panic!("{r:?}"),
        }
    }

    #[tokio::test]
    async fn push_handshake() {
        let (mut a, mut b) = duplex(1024);
        let downloader = tokio::spawn(async move {
            let r = accept(&mut a).await.unwrap();
            a.write_all(b"0").await.unwrap();
            r
        });
        expect_ack(&mut b).await.unwrap();
        send_push(&mut b, "alice", "/music/a b.mp3", 99).await.unwrap();
        assert_eq!(read_offset(&mut b).await.unwrap(), Ok(0));
        assert_eq!(
            downloader.await.unwrap(),
            PeerRequest::Send { nick: "alice".into(), filename: "/music/a b.mp3".into(), size: 99 }
        );
    }

    #[tokio::test]
    async fn list_roundtrip() {
        let files = vec![FileInfo {
            filename: "C:\\a.mp3".into(),
            md5: "d41d8cd98f00b204e9800998ecf8427e".into(),
            size: 10,
            bitrate: 128,
            frequency: 44100,
            seconds: 3,
        }];
        let bytes = encode_list("me", &files);
        let mut r = &bytes[..];
        let (nick, got) = read_list(&mut r, vec![], true).await.unwrap();
        assert_eq!(nick.as_deref(), Some("me"));
        assert_eq!(got, files);
    }

    #[test]
    fn request_line_parsing() {
        assert_eq!(parse_request_line(b"bob \"a b.mp3\" 12"), Some(("bob".into(), "a b.mp3".into(), 12)));
        assert_eq!(parse_request_line(b"bob \"a b.mp3"), None);
        assert_eq!(parse_request_line(b"bob \"a b.mp3\" "), None);
    }
}
