//! The built-in player ("My Files & Player" in the original): a playlist,
//! play/pause/stop/next/previous, seeking, volume, and M3U/PLS playlists.
//! Audio goes through rodio; MP3, WAV, FLAC and Ogg Vorbis play (the
//! original's WMA support came from Windows Media and isn't available).

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub seconds: u32,
}

impl Track {
    pub fn new(path: PathBuf) -> Self {
        let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let seconds = napster_core::library::describe(&path).ok().flatten().map(|i| i.seconds).unwrap_or(0);
        Self { path, title, seconds }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Playing,
    Paused,
}

pub struct Player {
    output: Option<(OutputStream, OutputStreamHandle)>,
    sink: Option<Sink>,
    pub playlist: Vec<Track>,
    pub current: Option<usize>,
    pub selected: Option<usize>,
    volume: f32,
    /// Length reported by the decoder, if any (more exact than our estimate).
    decoded_len: Option<Duration>,
    pub error: Option<String>,
}

impl Default for Player {
    fn default() -> Self {
        Self {
            output: None,
            sink: None,
            playlist: vec![],
            current: None,
            selected: None,
            volume: 0.8,
            decoded_len: None,
            error: None,
        }
    }
}

impl Player {
    pub fn state(&self) -> State {
        match &self.sink {
            Some(s) if s.is_paused() => State::Paused,
            Some(s) if !s.empty() => State::Playing,
            _ => State::Stopped,
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if let Some(s) = &self.sink {
            s.set_volume(self.volume);
        }
    }

    pub fn position(&self) -> Duration {
        self.sink.as_ref().map(|s| s.get_pos()).unwrap_or_default()
    }

    pub fn length(&self) -> Duration {
        self.decoded_len
            .or_else(|| self.current.and_then(|i| self.playlist.get(i)).map(|t| Duration::from_secs(t.seconds as u64)))
            .unwrap_or_default()
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.playlist.get(i))
    }

    /// Add a file and return its index (an existing entry is reused).
    pub fn enqueue(&mut self, path: PathBuf) -> usize {
        if let Some(i) = self.playlist.iter().position(|t| t.path == path) {
            return i;
        }
        self.playlist.push(Track::new(path));
        self.playlist.len() - 1
    }

    pub fn play_file(&mut self, path: PathBuf) {
        let i = self.enqueue(path);
        self.play_index(i);
    }

    pub fn play_index(&mut self, i: usize) {
        self.stop();
        let Some(track) = self.playlist.get(i).cloned() else {
            return;
        };
        self.current = Some(i);
        self.selected = Some(i);
        self.error = None;
        match self.start(&track.path) {
            Ok(()) => {}
            Err(e) => {
                self.error = Some(format!(
                    "[{}] could not be played: {e}. Check that you have a working playback device that is not \
                     in use by another player, and that the file is a valid MP3.",
                    track.title
                ));
            }
        }
    }

    fn start(&mut self, path: &Path) -> anyhow::Result<()> {
        if self.output.is_none() {
            self.output = Some(OutputStream::try_default()?);
        }
        let handle = &self.output.as_ref().unwrap().1;
        let sink = Sink::try_new(handle)?;
        let decoder = Decoder::new(skip_id3(File::open(path)?)?)?;
        self.decoded_len = decoder.total_duration();
        sink.set_volume(self.volume);
        sink.append(decoder);
        self.sink = Some(sink);
        Ok(())
    }

    /// Play button: resume, or start the selected/first track.
    pub fn play(&mut self) {
        match self.state() {
            State::Paused => {
                if let Some(s) = &self.sink {
                    s.play();
                }
            }
            State::Playing => {}
            State::Stopped => {
                if let Some(i) = self.selected.or(self.current).or((!self.playlist.is_empty()).then_some(0)) {
                    self.play_index(i);
                }
            }
        }
    }

    pub fn pause(&mut self) {
        if let Some(s) = &self.sink {
            if s.is_paused() {
                s.play();
            } else {
                s.pause();
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.decoded_len = None;
    }

    pub fn next(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        let i = self.current.map(|c| (c + 1) % self.playlist.len()).unwrap_or(0);
        self.play_index(i);
    }

    pub fn prev(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        // like most players: restart the song unless we're near its start
        if self.position() > Duration::from_secs(3) {
            if let Some(c) = self.current {
                return self.play_index(c);
            }
        }
        let n = self.playlist.len();
        let i = self.current.map(|c| (c + n - 1) % n).unwrap_or(0);
        self.play_index(i);
    }

    pub fn seek(&mut self, to: Duration) {
        if let Some(s) = &self.sink {
            if let Err(e) = s.try_seek(to) {
                tracing::debug!("seek failed: {e}");
            }
        }
    }

    pub fn remove(&mut self, i: usize) {
        if i >= self.playlist.len() {
            return;
        }
        if self.current == Some(i) {
            self.stop();
            self.current = None;
        }
        self.playlist.remove(i);
        let shift = |x: Option<usize>| match x {
            Some(c) if c > i => Some(c - 1),
            Some(c) if c == i => None,
            other => other,
        };
        self.current = shift(self.current);
        self.selected = shift(self.selected).filter(|&s| s < self.playlist.len());
    }

    pub fn clear(&mut self) {
        self.stop();
        self.playlist.clear();
        self.current = None;
        self.selected = None;
    }

    /// Call every frame: moves on to the next song when one finishes.
    pub fn tick(&mut self) {
        let finished = self.sink.as_ref().is_some_and(|s| s.empty() && !s.is_paused());
        if finished {
            self.sink = None;
            match self.current {
                Some(c) if c + 1 < self.playlist.len() => self.play_index(c + 1),
                _ => self.current = None,
            }
        }
    }

    pub fn load_playlist(&mut self, path: &Path) -> anyhow::Result<usize> {
        let text = std::fs::read_to_string(path)?;
        let base = path.parent().unwrap_or(Path::new("."));
        let entries = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pls")) {
            parse_pls(&text, base)
        } else {
            parse_m3u(&text, base)
        };
        let n = entries.len();
        for p in entries {
            self.enqueue(p);
        }
        Ok(n)
    }

    pub fn save_playlist(&self, path: &Path) -> anyhow::Result<()> {
        let text = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pls")) {
            write_pls(&self.playlist)
        } else {
            write_m3u(&self.playlist)
        };
        std::fs::write(path, text)?;
        Ok(())
    }
}

/// Open `file` positioned after any ID3v2 tag. The decoder chokes on some
/// tags (an empty one, for instance) and we don't use their metadata.
fn skip_id3(mut file: File) -> std::io::Result<Offset<BufReader<File>>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut head = [0u8; 10];
    let n = file.read(&mut head)?;
    let base = napster_core::mp3::id3v2_len(&head[..n]) as u64;
    let mut inner = BufReader::new(file);
    inner.seek(SeekFrom::Start(base))?;
    Ok(Offset { inner, base })
}

/// A reader whose position 0 is `base` bytes into the underlying one.
struct Offset<R> {
    inner: R,
    base: u64,
}

impl<R: std::io::Read> std::io::Read for Offset<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: std::io::Seek> std::io::Seek for Offset<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::SeekFrom;
        let at = match pos {
            SeekFrom::Start(n) => self.inner.seek(SeekFrom::Start(self.base + n))?,
            other => self.inner.seek(other)?,
        };
        if at < self.base {
            self.inner.seek(SeekFrom::Start(self.base))?;
            return Ok(0);
        }
        Ok(at - self.base)
    }
}

fn resolve(entry: &str, base: &Path) -> PathBuf {
    let p = PathBuf::from(entry.trim());
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

pub fn parse_m3u(text: &str, base: &Path) -> Vec<PathBuf> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| resolve(l, base)).collect()
}

pub fn parse_pls(text: &str, base: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<(u32, PathBuf)> = text
        .lines()
        .filter_map(|l| {
            let (k, v) = l.trim().split_once('=')?;
            let n = k.trim().strip_prefix("File")?.parse().ok()?;
            Some((n, resolve(v, base)))
        })
        .collect();
    entries.sort_by_key(|(n, _)| *n);
    entries.into_iter().map(|(_, p)| p).collect()
}

pub fn write_m3u(tracks: &[Track]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        out.push_str(&format!("#EXTINF:{},{}\n{}\n", t.seconds, t.title, t.path.display()));
    }
    out
}

pub fn write_pls(tracks: &[Track]) -> String {
    let mut out = String::from("[playlist]\n");
    for (i, t) in tracks.iter().enumerate() {
        let n = i + 1;
        out.push_str(&format!("File{n}={}\nTitle{n}={}\nLength{n}={}\n", t.path.display(), t.title, t.seconds));
    }
    out.push_str(&format!("NumberOfEntries={}\nVersion=2\n", tracks.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(p: &str) -> Track {
        Track { path: PathBuf::from(p), title: "t".into(), seconds: 60 }
    }

    #[test]
    fn m3u_roundtrip() {
        let base = Path::new("/music");
        let text = write_m3u(&[track("/music/a.mp3"), track("/music/b c.mp3")]);
        assert_eq!(parse_m3u(&text, base), vec![PathBuf::from("/music/a.mp3"), PathBuf::from("/music/b c.mp3")]);
        assert_eq!(parse_m3u("# comment\nsub/x.mp3\n\n", base), vec![base.join("sub/x.mp3")]);
    }

    #[test]
    fn pls_roundtrip_and_order() {
        let base = Path::new("/music");
        let text = write_pls(&[track("/music/a.mp3"), track("/music/b.mp3")]);
        assert_eq!(parse_pls(&text, base), vec![PathBuf::from("/music/a.mp3"), PathBuf::from("/music/b.mp3")]);
        let shuffled = "[playlist]\nFile2=two.mp3\nFile1=one.mp3\nTitle1=x\n";
        assert_eq!(parse_pls(shuffled, base), vec![base.join("one.mp3"), base.join("two.mp3")]);
    }

    #[test]
    fn decodes_mp3() {
        let dir = std::env::temp_dir().join(format!("napster-player-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.mp3");
        std::fs::write(&path, napster_core::mp3::synth_mp3(200, 9)).unwrap();
        let decoder = Decoder::new(BufReader::new(File::open(&path).unwrap())).unwrap();
        assert_eq!(decoder.sample_rate(), 44100);
        let t = Track::new(path);
        assert_eq!(t.title, "tone");
        assert_eq!(t.seconds, 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn decodes_mp3_with_id3_tag() {
        let dir = std::env::temp_dir().join(format!("napster-player-id3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tagged.mp3");
        let mut data = b"ID3      ".to_vec();
        data.extend(napster_core::mp3::synth_mp3(200, 9));
        std::fs::write(&path, data).unwrap();
        let decoder = Decoder::new(skip_id3(File::open(&path).unwrap()).unwrap());
        let _ = std::fs::remove_dir_all(dir);
        assert!(decoder.is_ok(), "{:?}", decoder.err());
    }

    #[test]
    fn playlist_editing() {
        let mut p = Player::default();
        for f in ["a.mp3", "b.mp3", "c.mp3"] {
            p.playlist.push(track(f));
        }
        p.current = Some(2);
        p.selected = Some(1);
        p.remove(1);
        assert_eq!(p.playlist.len(), 2);
        assert_eq!(p.current, Some(1));
        assert_eq!(p.selected, None);
        assert_eq!(p.enqueue(PathBuf::from("a.mp3")), 0);
        assert_eq!(p.state(), State::Stopped);
    }
}
