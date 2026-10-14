//! Artist / title / genre from ID3 tags (v2.2-2.4 and v1), falling back to
//! the "Artist - Title.mp3" file naming everyone used in 1999. Feeds the
//! Discover tab (genres, charts, members with similar taste).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMeta {
    pub artist: String,
    pub title: String,
    pub genre: String,
}

impl TrackMeta {
    pub fn is_empty(&self) -> bool {
        self.artist.is_empty() && self.title.is_empty() && self.genre.is_empty()
    }
}

/// The standard ID3v1 genres plus the common Winamp extensions.
pub const GENRES: [&str; 126] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "Alternative Rock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychedelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast Fusion",
    "Bebop",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A Cappella",
    "Euro-House",
    "Dance Hall",
];

/// "(17)", "(17)Rock", "17" or "Rock" -> "Rock"
pub fn normalize_genre(raw: &str) -> String {
    let raw = raw.trim().trim_matches('\0').trim();
    if let Some(rest) = raw.strip_prefix('(') {
        if let Some((num, after)) = rest.split_once(')') {
            let after = after.trim();
            if !after.is_empty() {
                return after.to_owned();
            }
            if let Some(g) = num.parse::<usize>().ok().and_then(|n| GENRES.get(n)) {
                return (*g).to_owned();
            }
            return match num {
                "RX" => "Remix".into(),
                "CR" => "Cover".into(),
                _ => String::new(),
            };
        }
    }
    if let Ok(n) = raw.parse::<usize>() {
        return GENRES.get(n).map(|g| (*g).to_owned()).unwrap_or_default();
    }
    raw.to_owned()
}

fn decode_text(enc: u8, b: &[u8]) -> String {
    let s = match enc {
        // UTF-16 with BOM / UTF-16BE
        1 | 2 => {
            let (body, be) = match b {
                [0xfe, 0xff, rest @ ..] => (rest, true),
                [0xff, 0xfe, rest @ ..] => (rest, false),
                rest => (rest, enc == 2),
            };
            let units: Vec<u16> = body
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| if be { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) })
                .collect();
            String::from_utf16_lossy(&units)
        }
        3 => String::from_utf8_lossy(b).into_owned(),
        // ISO-8859-1
        _ => b.iter().map(|&c| c as char).collect(),
    };
    // first string only (v2.4 allows several, NUL separated)
    s.split('\0').next().unwrap_or("").trim().to_owned()
}

fn syncsafe(b: &[u8]) -> usize {
    b.iter().fold(0usize, |acc, &x| (acc << 7) | (x & 0x7f) as usize)
}

/// Parse an ID3v2 tag at the start of `data` (the whole tag must be there).
pub fn parse_id3v2(data: &[u8]) -> TrackMeta {
    let mut meta = TrackMeta::default();
    if data.len() < 10 || &data[..3] != b"ID3" {
        return meta;
    }
    let version = data[3];
    let flags = data[5];
    let size = syncsafe(&data[6..10]);
    let end = (10 + size).min(data.len());
    let mut pos = 10;
    // skip an extended header
    if flags & 0x40 != 0 && version >= 3 && pos + 4 <= end {
        let ext = if version == 4 {
            syncsafe(&data[pos..pos + 4])
        } else {
            u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize + 4
        };
        pos += ext;
    }
    let (id_len, header_len) = if version == 2 { (3, 6) } else { (4, 10) };
    while pos + header_len <= end {
        let id = &data[pos..pos + id_len];
        if id[0] == 0 {
            break; // padding
        }
        let fsize = match version {
            2 => (data[pos + 3] as usize) << 16 | (data[pos + 4] as usize) << 8 | data[pos + 5] as usize,
            4 => syncsafe(&data[pos + 4..pos + 8]),
            _ => u32::from_be_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize,
        };
        let body_start = pos + header_len;
        let body_end = body_start + fsize;
        if fsize == 0 || body_end > end {
            break;
        }
        let body = &data[body_start..body_end];
        let text = || decode_text(body[0], &body[1..]);
        match id {
            b"TPE1" | b"TP1" if meta.artist.is_empty() => meta.artist = text(),
            b"TIT2" | b"TT2" if meta.title.is_empty() => meta.title = text(),
            b"TCON" | b"TCO" if meta.genre.is_empty() => meta.genre = normalize_genre(&text()),
            _ => {}
        }
        pos = body_end;
    }
    meta
}

/// Parse the 128-byte ID3v1 tag at the end of a file.
pub fn parse_id3v1(tail: &[u8]) -> TrackMeta {
    if tail.len() < 128 || &tail[tail.len() - 128..tail.len() - 125] != b"TAG" {
        return TrackMeta::default();
    }
    let t = &tail[tail.len() - 128..];
    let field = |b: &[u8]| b.iter().map(|&c| c as char).collect::<String>().trim_matches(['\0', ' ']).to_owned();
    TrackMeta {
        title: field(&t[3..33]),
        artist: field(&t[33..63]),
        genre: GENRES.get(t[127] as usize).map(|g| (*g).to_owned()).unwrap_or_default(),
    }
}

/// "Artist - Title.mp3" (also "01 - Artist - Title.mp3")
pub fn from_filename(name: &str) -> TrackMeta {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    let parts: Vec<&str> = stem.split(" - ").map(str::trim).filter(|p| !p.is_empty()).collect();
    let parts: Vec<&str> = match parts.as_slice() {
        [n, rest @ ..] if rest.len() >= 2 && n.chars().all(|c| c.is_ascii_digit()) => rest.to_vec(),
        p => p.to_vec(),
    };
    match parts.as_slice() {
        [artist, title, ..] => TrackMeta { artist: (*artist).into(), title: (*title).into(), genre: String::new() },
        [title] => TrackMeta { title: (*title).into(), ..TrackMeta::default() },
        [] => TrackMeta::default(),
    }
}

/// Everything we can learn about a file: its tags, then its name.
pub fn read(path: &Path) -> TrackMeta {
    let mut meta = TrackMeta::default();
    if let Ok(mut f) = File::open(path) {
        let mut head = [0u8; 10];
        if f.read_exact(&mut head).is_ok() && &head[..3] == b"ID3" {
            let size = 10 + syncsafe(&head[6..10]);
            // don't load giant embedded artwork just for three text frames
            let mut tag = vec![0u8; size.min(512 * 1024)];
            if f.seek(SeekFrom::Start(0)).is_ok() && f.read_exact(&mut tag).is_ok() {
                meta = parse_id3v2(&tag);
            }
        }
        if meta.artist.is_empty() || meta.title.is_empty() || meta.genre.is_empty() {
            let mut tail = [0u8; 128];
            if f.seek(SeekFrom::End(-128)).is_ok() && f.read_exact(&mut tail).is_ok() {
                let v1 = parse_id3v1(&tail);
                fill(&mut meta, v1);
            }
        }
    }
    if meta.artist.is_empty() || meta.title.is_empty() {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        fill(&mut meta, from_filename(&name));
    }
    meta
}

fn fill(meta: &mut TrackMeta, other: TrackMeta) {
    if meta.artist.is_empty() {
        meta.artist = other.artist;
    }
    if meta.title.is_empty() {
        meta.title = other.title;
    }
    if meta.genre.is_empty() {
        meta.genre = other.genre;
    }
}

/// Build an ID3v2.3 tag with artist/title/genre text frames (tests, demos).
pub fn id3v2_tag(artist: &str, title: &str, genre: &str) -> Vec<u8> {
    let mut frames = Vec::new();
    for (id, text) in [(b"TPE1", artist), (b"TIT2", title), (b"TCON", genre)] {
        if text.is_empty() {
            continue;
        }
        let mut body = vec![0u8]; // ISO-8859-1
        body.extend(text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }));
        frames.extend_from_slice(id);
        frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend_from_slice(&body);
    }
    let size = frames.len();
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend([(size >> 21) as u8 & 0x7f, (size >> 14) as u8 & 0x7f, (size >> 7) as u8 & 0x7f, size as u8 & 0x7f]);
    tag.extend(frames);
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres() {
        assert_eq!(normalize_genre("(17)"), "Rock");
        assert_eq!(normalize_genre("(17)Rock"), "Rock");
        assert_eq!(normalize_genre("17"), "Rock");
        assert_eq!(normalize_genre("Trip-Hop"), "Trip-Hop");
        assert_eq!(normalize_genre("(RX)"), "Remix");
        assert_eq!(normalize_genre("(999)"), "");
    }

    #[test]
    fn v23_roundtrip() {
        let tag = id3v2_tag("Velvet Arcade", "Golden Hour", "(20)");
        let m = parse_id3v2(&tag);
        assert_eq!(
            m,
            TrackMeta { artist: "Velvet Arcade".into(), title: "Golden Hour".into(), genre: "Alternative".into() }
        );
    }

    #[test]
    fn v24_utf8_and_v22() {
        // v2.4: syncsafe frame size, UTF-8 text
        let body = [&[3u8][..], "Björk".as_bytes()].concat();
        let mut frame = b"TPE1".to_vec();
        frame.extend([0, 0, 0, body.len() as u8, 0, 0]);
        frame.extend(&body);
        let mut tag = b"ID3\x04\x00\x00\x00\x00\x00".to_vec();
        tag.push(frame.len() as u8);
        tag.extend(&frame);
        assert_eq!(parse_id3v2(&tag).artist, "Björk");
        // v2.2: 3-byte ids and sizes
        let mut tag = b"ID3\x02\x00\x00\x00\x00\x00".to_vec();
        let f = [b"TT2".as_slice(), &[0, 0, 5], &[0], b"Song"].concat();
        tag.push(f.len() as u8);
        tag.extend(&f);
        assert_eq!(parse_id3v2(&tag).title, "Song");
    }

    #[test]
    fn v1_and_filename_fallback() {
        let mut t = vec![0u8; 128];
        t[..3].copy_from_slice(b"TAG");
        t[3..9].copy_from_slice(b"Title1");
        t[33..40].copy_from_slice(b"Artist1");
        t[127] = 7;
        assert_eq!(
            parse_id3v1(&t),
            TrackMeta { artist: "Artist1".into(), title: "Title1".into(), genre: "Hip-Hop".into() }
        );
        assert_eq!(from_filename("The Midnight Static - Neon Highway.mp3").artist, "The Midnight Static");
        assert_eq!(from_filename("03 - Pixel Garden - 56k Dreams.mp3").title, "56k Dreams");
        assert_eq!(from_filename("untitled.mp3").title, "untitled");
    }

    #[test]
    fn reads_files() {
        let dir = std::env::temp_dir().join(format!("napster-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tagged = dir.join("x.mp3");
        let mut data = id3v2_tag("A", "B", "Jazz");
        data.extend(crate::mp3::synth_mp3(10, 9));
        std::fs::write(&tagged, data).unwrap();
        assert_eq!(read(&tagged), TrackMeta { artist: "A".into(), title: "B".into(), genre: "Jazz".into() });
        let untagged = dir.join("Some Band - Some Song.mp3");
        std::fs::write(&untagged, crate::mp3::synth_mp3(10, 9)).unwrap();
        let m = read(&untagged);
        assert_eq!((m.artist.as_str(), m.title.as_str(), m.genre.as_str()), ("Some Band", "Some Song", ""));
        let _ = std::fs::remove_dir_all(dir);
    }
}
