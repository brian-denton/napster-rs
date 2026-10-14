//! Just enough MPEG audio parsing to fill in the share message: bitrate,
//! sample rate, duration, and where the audio starts (for the MD5).
//! This replaces vmp3.nvm, the validator DLL the original client loaded.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub mpeg1: bool,
    pub layer: u8,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub padding: bool,
    pub mono: bool,
    pub version25: bool,
}

const BITRATES_V1: [[u32; 15]; 3] = [
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448], // layer I
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],    // layer II
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],     // layer III
];
const BITRATES_V2: [[u32; 15]; 2] = [
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256], // layer I
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],      // layer II & III
];

impl FrameHeader {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
            return None;
        }
        let version = (b[1] >> 3) & 3; // 0 = 2.5, 2 = 2, 3 = 1
        let layer_bits = (b[1] >> 1) & 3; // 1 = III, 2 = II, 3 = I
        if version == 1 || layer_bits == 0 {
            return None;
        }
        let layer = 4 - layer_bits;
        let br_idx = (b[2] >> 4) as usize;
        let sr_idx = ((b[2] >> 2) & 3) as usize;
        if br_idx == 0 || br_idx == 15 || sr_idx == 3 {
            return None;
        }
        let mpeg1 = version == 3;
        let bitrate = if mpeg1 {
            BITRATES_V1[layer as usize - 1][br_idx]
        } else {
            BITRATES_V2[if layer == 1 { 0 } else { 1 }][br_idx]
        };
        let base = [44100, 48000, 32000][sr_idx];
        let sample_rate = match version {
            3 => base,
            2 => base / 2,
            _ => base / 4,
        };
        Some(Self {
            mpeg1,
            layer,
            bitrate,
            sample_rate,
            padding: b[2] & 2 != 0,
            mono: (b[3] >> 6) == 3,
            version25: version == 0,
        })
    }

    pub fn samples_per_frame(&self) -> u32 {
        match (self.layer, self.mpeg1) {
            (1, _) => 384,
            (2, _) => 1152,
            (_, true) => 1152,
            (_, false) => 576,
        }
    }

    pub fn frame_len(&self) -> usize {
        let pad = self.padding as u32;
        let len = if self.layer == 1 {
            (12 * self.bitrate * 1000 / self.sample_rate + pad) * 4
        } else {
            self.samples_per_frame() / 8 * self.bitrate * 1000 / self.sample_rate + pad
        };
        len as usize
    }

    /// Offset of a Xing/Info VBR header from the frame start.
    fn xing_offset(&self) -> usize {
        4 + match (self.mpeg1, self.mono) {
            (true, false) => 32,
            (true, true) => 17,
            (false, false) => 17,
            (false, true) => 9,
        }
    }
}

/// Size of a leading ID3v2 tag (0 if none).
pub fn id3v2_len(b: &[u8]) -> usize {
    if b.len() >= 10 && &b[..3] == b"ID3" {
        let size = b[6..10].iter().fold(0usize, |acc, &x| (acc << 7) | (x & 0x7f) as usize);
        let footer = if b[5] & 0x10 != 0 { 10 } else { 0 };
        10 + size + footer
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInfo {
    /// Byte offset of the first frame.
    pub start: usize,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub seconds: u32,
}

/// Find the first frame whose successor is also a valid frame, starting at
/// `from`. Requiring two consecutive headers avoids false syncs in tags.
pub fn find_sync(b: &[u8], from: usize) -> Option<(usize, FrameHeader)> {
    let mut i = from;
    while i + 4 <= b.len() {
        if let Some(h) = FrameHeader::parse(&b[i..]) {
            let next = i + h.frame_len();
            match b.get(next..next + 4) {
                Some(nb) => {
                    if FrameHeader::parse(nb).is_some_and(|n| n.sample_rate == h.sample_rate) {
                        return Some((i, h));
                    }
                }
                // can't verify past the buffer; accept
                None => return Some((i, h)),
            }
        }
        i += 1;
    }
    None
}

/// Analyze the head of a file. `head` is the first bytes of the file
/// (64 KiB is plenty past any ID3 tag) and `file_len` its total size.
pub fn analyze(head: &[u8], file_len: u64) -> Option<AudioInfo> {
    let tag = id3v2_len(head);
    let (start, h) = find_sync(head, tag.min(head.len()))?;
    let audio_len = file_len.saturating_sub(start as u64);
    // VBR: Xing/Info header with a frame count
    let xo = start + h.xing_offset();
    if let Some(x) = head.get(xo..xo + 12) {
        if &x[..4] == b"Xing" || &x[..4] == b"Info" {
            let flags = u32::from_be_bytes([x[4], x[5], x[6], x[7]]);
            if flags & 1 != 0 {
                let frames = u32::from_be_bytes([x[8], x[9], x[10], x[11]]) as u64;
                let secs = frames * h.samples_per_frame() as u64 / h.sample_rate as u64;
                let bitrate = (audio_len * 8).checked_div(secs).map(|b| (b / 1000) as u32).unwrap_or(h.bitrate);
                return Some(AudioInfo { start, bitrate, sample_rate: h.sample_rate, seconds: secs as u32 });
            }
        }
    }
    let seconds = (audio_len * 8 / (h.bitrate as u64 * 1000)) as u32;
    Some(AudioInfo { start, bitrate: h.bitrate, sample_rate: h.sample_rate, seconds })
}

/// Build a silent-ish CBR MPEG-1 layer III stream (for tests and demos).
pub fn synth_mp3(frames: usize, bitrate_idx: u8) -> Vec<u8> {
    let header = [0xff, 0xfb, (bitrate_idx << 4), 0x00];
    let h = FrameHeader::parse(&header).expect("valid header");
    let mut out = Vec::with_capacity(frames * h.frame_len());
    for i in 0..frames {
        out.extend_from_slice(&header);
        out.extend((4..h.frame_len()).map(|j| ((i * 31 + j * 7) % 251) as u8));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_128k_44100() {
        let h = FrameHeader::parse(&[0xff, 0xfb, 0x90, 0x00]).unwrap();
        assert_eq!((h.layer, h.bitrate, h.sample_rate), (3, 128, 44100));
        assert_eq!(h.frame_len(), 417);
    }

    #[test]
    fn analyze_cbr_with_id3() {
        let mut data = b"ID3\x03\x00\x00\x00\x00\x00\x0a".to_vec();
        data.extend_from_slice(&[0u8; 10]);
        let audio = synth_mp3(1000, 9);
        data.extend_from_slice(&audio);
        let info = analyze(&data[..65536], data.len() as u64).unwrap();
        assert_eq!(info.start, 20);
        assert_eq!(info.bitrate, 128);
        assert_eq!(info.sample_rate, 44100);
        // 1000 frames * 1152 / 44100 ~= 26.1s
        assert_eq!(info.seconds, 26);
    }

    #[test]
    fn rejects_noise() {
        assert!(analyze(&[0u8; 4096], 4096).is_none());
    }
}
