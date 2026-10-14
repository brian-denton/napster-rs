//! The shared-file library: scanning folders, hashing, and a small cache so
//! rescans don't rehash unchanged files (the original kept `shared.dat`).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use md5::{Digest, Md5};
use napster_proto::FileInfo;
use serde::{Deserialize, Serialize};

use crate::mp3;

/// The Windows client hashes this many bytes, starting at the first frame.
pub const HASH_LEN: usize = 299_008;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    pub info: FileInfo,
    pub path: PathBuf,
}

#[derive(Debug, Default)]
pub struct Library {
    files: Vec<SharedFile>,
    by_name: HashMap<String, usize>,
}

impl Library {
    pub fn new(files: Vec<SharedFile>) -> Self {
        let by_name = files.iter().enumerate().map(|(i, f)| (f.info.filename.clone(), i)).collect();
        Self { files, by_name }
    }

    pub fn files(&self) -> &[SharedFile] {
        &self.files
    }

    pub fn infos(&self) -> Vec<FileInfo> {
        self.files.iter().map(|f| f.info.clone()).collect()
    }

    /// Look up by the exact name we advertised. Only these paths are ever
    /// served, so a peer can't request arbitrary files.
    pub fn get(&self, filename: &str) -> Option<&SharedFile> {
        self.by_name.get(filename).map(|&i| &self.files[i])
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    md5: String,
    bitrate: u32,
    frequency: u32,
    seconds: u32,
}

fn cache_path() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("napster-rs").join("library.json")
}

/// Describe one file, or `None` if it isn't valid audio.
pub fn describe(path: &Path) -> std::io::Result<Option<FileInfo>> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let filename = path.to_string_lossy().into_owned();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "wma" {
        // newer clients send WMA-FILE instead of a hash for Windows Media
        return Ok(Some(FileInfo { filename, md5: "WMA-FILE".into(), size, bitrate: 0, frequency: 0, seconds: 0 }));
    }
    let mut head = vec![0u8; 64 * 1024];
    let n = read_up_to(&mut f, &mut head)?;
    head.truncate(n);
    let tag = mp3::id3v2_len(&head);
    // big ID3 tags (embedded art) push the audio past our window
    if tag + 8 > head.len() && (tag as u64) < size {
        f.seek(SeekFrom::Start(tag as u64))?;
        let mut more = vec![0u8; 64 * 1024];
        let m = read_up_to(&mut f, &mut more)?;
        let mut shifted = vec![0u8; tag];
        shifted.extend_from_slice(&more[..m]);
        head = shifted;
    }
    let is_mpeg = matches!(ext.as_str(), "mp3" | "mp2" | "mp1" | "mpga");
    let info = if is_mpeg { mp3::analyze(&head, size) } else { None };
    if is_mpeg && info.is_none() {
        return Ok(None);
    }
    let start = info.map(|i| i.start).unwrap_or(0) as u64;
    f.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; HASH_LEN];
    let n = read_up_to(&mut f, &mut buf)?;
    let md5 = hex(&Md5::digest(&buf[..n]));
    let (bitrate, frequency, seconds) = info.map(|i| (i.bitrate, i.sample_rate, i.seconds)).unwrap_or((0, 0, 0));
    Ok(Some(FileInfo { filename, md5, size, bitrate, frequency, seconds }))
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn walk(dir: &Path, exts: &[String], out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            // same exclusions as the original drive scan
            if name == "recycle"
                || name == "$recycle.bin"
                || name == "system volume information"
                || name == "incomplete"
            {
                continue;
            }
            walk(&path, exts, out, depth + 1);
        } else if ft.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
                out.push(path);
            }
        }
    }
}

/// Scan `dirs` for shareable files. `progress(done, total)` is called as
/// files are hashed. Blocking; run it on a blocking thread.
pub fn scan(dirs: &[PathBuf], exts: &[String], use_cache: bool, mut progress: impl FnMut(usize, usize)) -> Library {
    let mut paths = Vec::new();
    for d in dirs {
        walk(d, exts, &mut paths, 0);
    }
    paths.sort();
    paths.dedup();
    let mut cache: HashMap<String, CacheEntry> = if use_cache {
        std::fs::read(cache_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    } else {
        HashMap::new()
    };
    let mut files = Vec::with_capacity(paths.len());
    let total = paths.len();
    for (i, path) in paths.into_iter().enumerate() {
        progress(i, total);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        let mtime =
            meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
        let key = path.to_string_lossy().into_owned();
        if let Some(c) = cache.get(&key).filter(|c| c.size == meta.len() && c.mtime == mtime) {
            files.push(SharedFile {
                info: FileInfo {
                    filename: key,
                    md5: c.md5.clone(),
                    size: c.size,
                    bitrate: c.bitrate,
                    frequency: c.frequency,
                    seconds: c.seconds,
                },
                path,
            });
            continue;
        }
        match describe(&path) {
            Ok(Some(info)) => {
                cache.insert(
                    key,
                    CacheEntry {
                        size: info.size,
                        mtime,
                        md5: info.md5.clone(),
                        bitrate: info.bitrate,
                        frequency: info.frequency,
                        seconds: info.seconds,
                    },
                );
                files.push(SharedFile { info, path });
            }
            Ok(None) => tracing::debug!("skipping {}: not valid audio", path.display()),
            Err(e) => tracing::debug!("skipping {}: {e}", path.display()),
        }
    }
    progress(total, total);
    if use_cache {
        let p = cache_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(b) = serde_json::to_vec(&cache) {
            let _ = std::fs::write(p, b);
        }
    }
    Library::new(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_and_describe() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Artist");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("Song.mp3"), mp3::synth_mp3(2000, 9)).unwrap();
        std::fs::write(sub.join("notes.txt"), b"hi").unwrap();
        std::fs::write(sub.join("broken.mp3"), [0u8; 5000]).unwrap();
        let lib = scan(&[dir.path().to_path_buf()], &["mp3".into()], false, |_, _| {});
        assert_eq!(lib.len(), 1);
        let f = &lib.files()[0];
        assert_eq!(f.info.bitrate, 128);
        assert_eq!(f.info.frequency, 44100);
        assert_eq!(f.info.seconds, 52);
        assert_eq!(f.info.md5.len(), 32);
        assert!(lib.get(&f.info.filename).is_some());
    }
}
