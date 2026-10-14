//! Peer connections: the data-port listener, downloads, uploads (normal and
//! firewalled/pushed) and direct browsing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use napster_proto::peer::{self, PeerRequest, SizeReply};
use napster_proto::{msg, FileInfo, SearchResult};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::engine::Shared;
use crate::types::*;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// No data for this long kills a transfer.
const STALL_TIMEOUT: Duration = Duration::from_secs(90);
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const CHUNK: usize = 64 * 1024;

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) async fn listen(shared: Arc<Shared>, listener: TcpListener, cancel: CancellationToken) {
    tracing::info!("data port listening on {:?}", listener.local_addr());
    loop {
        let accepted = tokio::select! {
            _ = cancel.cancelled() => return,
            a = listener.accept() => a,
        };
        match accepted {
            Ok((stream, peer)) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_incoming(shared, stream).await {
                        tracing::debug!("peer {peer}: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("data port accept: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn handle_incoming(shared: Arc<Shared>, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    match peer::accept(&mut stream).await? {
        PeerRequest::Get { nick, filename, offset } => serve_upload(shared, stream, nick, filename, offset).await,
        PeerRequest::Send { nick, filename, size } => receive_push(shared, stream, nick, filename, size).await,
        PeerRequest::GetList => {
            let files = shared.library().infos();
            stream.write_all(&peer::encode_list(&shared.my_nick(), &files)).await?;
            stream.shutdown().await
        }
        PeerRequest::SendList { nick, leftover } => {
            if !lock(&shared.pending_browses).remove(&nick.to_lowercase()) {
                return Ok(());
            }
            let ip = stream.peer_addr().ok().and_then(|a| match a.ip() {
                std::net::IpAddr::V4(v4) => Some(v4),
                _ => None,
            });
            let (_, files) = peer::read_list(&mut stream, leftover, false).await?;
            emit_list(&shared, &nick, ip, files);
            Ok(())
        }
    }
}

fn emit_list(shared: &Shared, nick: &str, ip: Option<std::net::Ipv4Addr>, files: Vec<FileInfo>) {
    for file in files {
        shared.emit(Event::BrowseResult(SearchResult {
            file,
            nick: nick.to_owned(),
            ip: ip.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
            link_speed: 0,
        }));
    }
    shared.emit(Event::BrowseEnd { nick: nick.to_owned() });
}

// ------------------------------------------------------------------ uploads

/// Someone connected to our data port and sent GET.
async fn serve_upload(
    shared: Arc<Shared>,
    mut stream: TcpStream,
    nick: String,
    filename: String,
    offset: u64,
) -> std::io::Result<()> {
    let lib = shared.library();
    let Some(shared_file) = lib.get(&filename) else {
        stream.write_all(peer::FILE_NOT_SHARED.as_bytes()).await?;
        return stream.shutdown().await;
    };
    if shared.config().max_uploads == 0 {
        stream.write_all(peer::INVALID_REQUEST.as_bytes()).await?;
        return stream.shutdown().await;
    }
    let path = shared_file.path.clone();
    let md5 = shared_file.info.md5.clone();
    let mut file = match File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            stream.write_all(peer::FILE_NOT_FOUND.as_bytes()).await?;
            return stream.shutdown().await;
        }
    };
    let size = file.metadata().await?.len();
    if offset > size {
        stream.write_all(peer::INVALID_REQUEST.as_bytes()).await?;
        return stream.shutdown().await;
    }
    let (id, cancel) =
        shared.new_transfer(Direction::Upload, &nick, &filename, size, &md5, TransferState::Transferring);
    stream.write_all(size.to_string().as_bytes()).await?;
    stream.flush().await?;
    send_file(&shared, id, cancel, &mut stream, &mut file, offset, size).await;
    Ok(())
}

/// 501: we're firewalled, so connect to the downloader and push.
pub(crate) async fn push_upload(shared: Arc<Shared>, nick: String, addr: SocketAddr, filename: String) {
    let lib = shared.library();
    let Some(shared_file) = lib.get(&filename) else { return };
    let (path, md5, size) = (shared_file.path.clone(), shared_file.info.md5.clone(), shared_file.info.size);
    let (id, cancel) =
        shared.new_transfer(Direction::Upload, &nick, &filename, size, &md5, TransferState::Connecting);
    let result: anyhow::Result<()> = async {
        let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
        stream.set_nodelay(true)?;
        peer::expect_ack(&mut stream).await?;
        let mut file = File::open(&path).await?;
        let size = file.metadata().await?.len();
        peer::send_push(&mut stream, &shared.my_nick(), &filename, size).await?;
        let offset = match peer::read_offset(&mut stream).await? {
            Ok(o) if o <= size => o,
            Ok(_) => anyhow::bail!("peer asked for an offset past the end"),
            Err(text) => anyhow::bail!("peer refused: {text}"),
        };
        shared.update(id, |t| {
            t.state = TransferState::Transferring;
            t.size = size;
        });
        send_file(&shared, id, cancel.clone(), &mut stream, &mut file, offset, size).await;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        shared.fail(id, format!("{e:#}"));
    }
}

async fn send_file<W: AsyncWrite + Unpin>(
    shared: &Shared,
    id: u64,
    cancel: CancellationToken,
    out: &mut W,
    file: &mut File,
    offset: u64,
    size: u64,
) {
    shared.send(msg::UPLOAD_START, "");
    shared.update(id, |t| {
        t.offset = offset;
        t.started = Some(Instant::now());
    });
    let result: std::io::Result<u64> = async {
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut buf = vec![0u8; CHUNK];
        let mut sent = 0u64;
        let mut last = Instant::now();
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(std::io::Error::other("cancelled")),
                r = timeout(STALL_TIMEOUT, out.write_all(&buf[..n])) => {
                    r.map_err(|_| std::io::Error::other("timed out sending data"))??
                }
            }
            sent += n as u64;
            if last.elapsed() > PROGRESS_EVERY {
                last = Instant::now();
                shared.update(id, |t| t.transferred = sent);
            }
        }
        out.flush().await?;
        Ok(sent)
    }
    .await;
    shared.send(msg::UPLOAD_COMPLETE, "");
    match result {
        Ok(sent) if offset + sent >= size => shared.update(id, |t| {
            t.transferred = sent;
            t.state = TransferState::Complete;
        }),
        Ok(sent) => shared.update(id, |t| {
            t.transferred = sent;
            t.state = TransferState::Failed("file shrank during upload".into());
        }),
        Err(e) => shared.fail(id, format!("{e}")),
    }
}

// ---------------------------------------------------------------- downloads

/// Partial and final paths for a download.
fn download_paths(shared: &Shared, filename: &str) -> (PathBuf, PathBuf) {
    let cfg = shared.config();
    let name = sanitize(napster_proto::share::basename(filename));
    (cfg.incomplete_dir().join(&name), cfg.download_dir.join(&name))
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let s = s.trim().trim_matches('.').to_owned();
    if s.is_empty() {
        "download".into()
    } else {
        s
    }
}

/// `dir/name.ext`, or `dir/name (n).ext` if taken.
fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_owned();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let ext = path.extension().and_then(|s| s.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    (1..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some free name")
}

async fn partial_len(path: &Path) -> u64 {
    tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0)
}

/// 204 with a real port: connect and GET.
pub(crate) async fn download(shared: Arc<Shared>, id: u64, nick: String, addr: SocketAddr, filename: String) {
    let Some(info) = shared.transfer(id) else { return };
    let cancel = lock(&shared.transfers).map.get(&id).map(|e| e.cancel.clone()).unwrap_or_default();
    shared.update(id, |t| t.state = TransferState::Connecting);
    let result: anyhow::Result<()> = async {
        let mut stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => s,
            _ => {
                // tell the server (and through it the uploader)
                shared.send(msg::DATA_PORT_ERROR, nick.clone());
                anyhow::bail!("could not connect to {nick}'s data port");
            }
        };
        stream.set_nodelay(true)?;
        peer::expect_ack(&mut stream).await?;
        let (partial, _) = download_paths(&shared, &filename);
        let mut offset = partial_len(&partial).await;
        if info.size > 0 && offset >= info.size {
            offset = 0;
        }
        peer::send_get(&mut stream, &shared.my_nick(), &filename, offset).await?;
        let expected = (info.size > 0).then_some(info.size);
        let (size, leftover) = match peer::read_size(&mut stream, expected).await? {
            SizeReply::Size(n, rest) => (n, rest),
            SizeReply::Error(text) => anyhow::bail!("{text}"),
        };
        if size == 0 {
            anyhow::bail!("File '{}' has a 0 byte file size!", info.basename());
        }
        receive_data(&shared, id, cancel.clone(), stream, leftover, &filename, offset, size).await
    }
    .await;
    if let Err(e) = result {
        shared.fail(id, format!("{e:#}"));
    }
    shared.pump_downloads();
}

/// Incoming SEND: a firewalled uploader we asked (500) is pushing a file.
async fn receive_push(
    shared: Arc<Shared>,
    mut stream: TcpStream,
    nick: String,
    filename: String,
    size: u64,
) -> std::io::Result<()> {
    let id = lock(&shared.pending_pushes).remove(&(nick.to_lowercase(), filename.clone()));
    let Some(id) = id else {
        stream.write_all(peer::INVALID_REQUEST.as_bytes()).await?;
        return stream.shutdown().await;
    };
    let cancel = lock(&shared.transfers).map.get(&id).map(|e| e.cancel.clone()).unwrap_or_default();
    let (partial, _) = download_paths(&shared, &filename);
    let mut offset = partial_len(&partial).await;
    if offset >= size {
        offset = 0;
    }
    stream.write_all(offset.to_string().as_bytes()).await?;
    stream.flush().await?;
    if let Err(e) = receive_data(&shared, id, cancel, stream, Vec::new(), &filename, offset, size).await {
        shared.fail(id, format!("{e:#}"));
    }
    shared.pump_downloads();
    Ok(())
}

/// Write the incoming stream into Incomplete/, then move it into place.
#[allow(clippy::too_many_arguments)]
async fn receive_data(
    shared: &Shared,
    id: u64,
    cancel: CancellationToken,
    mut stream: TcpStream,
    leftover: Vec<u8>,
    filename: &str,
    offset: u64,
    size: u64,
) -> anyhow::Result<()> {
    let (partial, final_path) = download_paths(shared, filename);
    if let Some(dir) = partial.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut file = OpenOptions::new().create(true).write(true).truncate(offset == 0).open(&partial).await?;
    file.set_len(offset).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;

    shared.send(msg::DOWNLOAD_START, "");
    shared.update(id, |t| {
        t.state = TransferState::Transferring;
        t.size = size;
        t.offset = offset;
        t.transferred = 0;
        t.started = Some(Instant::now());
    });
    let want = size - offset;
    let mut got = 0u64;
    let take = leftover.len().min(want as usize);
    file.write_all(&leftover[..take]).await?;
    got += take as u64;
    let mut buf = vec![0u8; CHUNK];
    let mut last = Instant::now();
    let result: anyhow::Result<()> = async {
        while got < want {
            let n = tokio::select! {
                _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                r = timeout(STALL_TIMEOUT, stream.read(&mut buf)) => r.map_err(|_| anyhow::anyhow!("timed out waiting for data"))??,
            };
            if n == 0 {
                anyhow::bail!("TCP socket went down at: {} bytes", offset + got);
            }
            let n = n.min((want - got) as usize);
            file.write_all(&buf[..n]).await?;
            got += n as u64;
            if last.elapsed() > PROGRESS_EVERY {
                last = Instant::now();
                shared.update(id, |t| t.transferred = got);
            }
        }
        Ok(())
    }
    .await;
    file.flush().await?;
    drop(file);
    shared.send(msg::DOWNLOAD_COMPLETE, "");
    result?;
    let dest = unique_path(&final_path);
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    if tokio::fs::rename(&partial, &dest).await.is_err() {
        tokio::fs::copy(&partial, &dest).await?;
        let _ = tokio::fs::remove_file(&partial).await;
    }
    tracing::info!("downloaded {} -> {}", filename, dest.display());
    shared.update(id, |t| {
        t.transferred = got;
        t.state = TransferState::Complete;
        t.saved_to = Some(dest);
    });
    Ok(())
}

// ------------------------------------------------------------ direct browse

/// 641: connect to the browsee and GETLIST.
pub(crate) async fn fetch_list(shared: Arc<Shared>, nick: String, addr: SocketAddr) {
    let result: anyhow::Result<()> = async {
        let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
        peer::expect_ack(&mut stream).await?;
        stream.write_all(peer::GETLIST).await?;
        stream.flush().await?;
        let (_, files) = peer::read_list(&mut stream, Vec::new(), true).await?;
        lock(&shared.pending_browses).remove(&nick.to_lowercase());
        let ip = match addr.ip() {
            std::net::IpAddr::V4(v4) => Some(v4),
            _ => None,
        };
        emit_list(&shared, &nick, ip, files);
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("direct browse of {nick} failed: {e:#}");
        shared.emit(Event::Info(format!("Direct browse failed. Requesting a Server list for user: {nick} ...")));
        shared.send(msg::BROWSE, nick);
    }
}

/// 640 with an address: we're firewalled, push our list (SENDLIST).
pub(crate) async fn push_list(shared: Arc<Shared>, addr: SocketAddr) {
    let result: anyhow::Result<()> = async {
        let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await??;
        peer::expect_ack(&mut stream).await?;
        let mut out = format!("SENDLIST {}\n", shared.my_nick()).into_bytes();
        let list = peer::encode_list("", &shared.library().infos());
        // encode_list starts with "<nick>\n"; SENDLIST already named us
        out.extend_from_slice(&list[1..]);
        stream.write_all(&out).await?;
        stream.shutdown().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        tracing::debug!("push list to {addr} failed: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("a/b:c?.mp3"), "a_b_c_.mp3");
        assert_eq!(sanitize(".."), "download");
    }

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.mp3");
        assert_eq!(unique_path(&p), p);
        std::fs::write(&p, b"1").unwrap();
        assert_eq!(unique_path(&p), dir.path().join("x (1).mp3"));
    }
}
