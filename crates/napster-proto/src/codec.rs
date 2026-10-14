//! Length-prefixed message framing.

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// The length field is 16 bits, so a payload can never exceed this.
pub const MAX_PAYLOAD: usize = u16::MAX as usize;

/// One protocol message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: u16,
    pub data: String,
}

impl Message {
    pub fn new(kind: u16, data: impl Into<String>) -> Self {
        Self { kind, data: data.into() }
    }

    pub fn empty(kind: u16) -> Self {
        Self { kind, data: String::new() }
    }

    pub fn args(&self) -> crate::Args<'_> {
        crate::Args::new(&self.data)
    }

    /// Encode as raw bytes (header + payload).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = BytesMut::new();
        NapCodec.encode(self.clone(), &mut buf).expect("encodable");
        buf.to_vec()
    }
}

/// Decode payload text. The original client is a Windows-1252 program, so
/// anything that isn't valid UTF-8 is treated as Latin-1 rather than lost.
pub fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("payload of {0} bytes exceeds the 65535 byte limit")]
    TooLong(usize),
}

/// `tokio_util` codec for [`Message`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NapCodec;

impl Decoder for NapCodec {
    type Item = Message;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Message>, CodecError> {
        if src.len() < 4 {
            return Ok(None);
        }
        let len = u16::from_le_bytes([src[0], src[1]]) as usize;
        if src.len() < 4 + len {
            src.reserve(4 + len - src.len());
            return Ok(None);
        }
        let kind = u16::from_le_bytes([src[2], src[3]]);
        src.advance(4);
        let payload = src.split_to(len);
        Ok(Some(Message { kind, data: decode_text(&payload) }))
    }
}

impl Encoder<Message> for NapCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), CodecError> {
        let payload = item.data.as_bytes();
        if payload.len() > MAX_PAYLOAD {
            return Err(CodecError::TooLong(payload.len()));
        }
        dst.reserve(4 + payload.len());
        dst.put_u16_le(payload.len() as u16);
        dst.put_u16_le(item.kind);
        dst.put_slice(payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let m = Message::new(2, "foo badpass 6699 \"nap v0.8\" 3");
        let bytes = m.to_bytes();
        assert_eq!(&bytes[..4], &[29, 0, 2, 0]);
        let mut buf = BytesMut::from(&bytes[..]);
        assert_eq!(NapCodec.decode(&mut buf).unwrap(), Some(m));
        assert!(buf.is_empty());
    }

    #[test]
    fn partial_frames() {
        let a = Message::new(205, "bob hi there").to_bytes();
        let b = Message::empty(202).to_bytes();
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut buf = BytesMut::new();
        let mut out = vec![];
        for byte in all {
            buf.put_u8(byte);
            while let Some(m) = NapCodec.decode(&mut buf).unwrap() {
                out.push(m);
            }
        }
        assert_eq!(out, vec![Message::new(205, "bob hi there"), Message::empty(202)]);
    }

    #[test]
    fn latin1_fallback() {
        assert_eq!(decode_text(&[0x42, 0x6a, 0xf6, 0x72, 0x6b]), "Björk");
    }
}
