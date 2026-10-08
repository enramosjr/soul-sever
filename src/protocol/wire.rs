//! Little-endian Soulseek primitives and length-prefixed frames.
//!
//! A server frame is `uint32 size`, `uint32 code`, then `size - 4` payload bytes.
//! `size` counts the code and the payload. It does not count itself.

use std::net::Ipv4Addr;

use thiserror::Error;

/// Largest server message Nicotine+ will accept (448 MiB, with headroom for share lists).
pub const MAX_MESSAGE_448M: u32 = 469_762_048;
/// Nicotine+ medium incoming-message cap.
pub const MAX_MESSAGE_16M: u32 = 16_777_216;
/// Nicotine+ 1 MiB incoming-message cap.
pub const MAX_MESSAGE_1M: u32 = 1_048_576;
/// Nicotine+ 16 KiB incoming-message cap.
pub const MAX_MESSAGE_16K: u32 = 16_384;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("message truncated: need {needed} bytes, have {available}")]
    Truncated { needed: usize, available: usize },
    #[error("message size {size} exceeds the {max} byte limit")]
    MessageTooLarge { size: u32, max: u32 },
    #[error("message size {size} is smaller than the 4 byte code")]
    MessageTooSmall { size: u32 },
    #[error("peer init size {size} is below the 5 byte minimum")]
    PeerInitTooSmall { size: u32 },
    #[error("distributed message size {size} is below the 5 byte minimum")]
    DistribTooSmall { size: u32 },
    #[error("unknown peer init type {code}")]
    UnknownPeerInit { code: u8 },
    #[error("peer connection type {value:?} is not P, F, or D")]
    UnknownConnectionType { value: String },
    #[error("peer username is rejected")]
    RejectedUsername,
    #[error("integer does not fit in the protocol field")]
    IntegerOverflow,
    #[error("shared file list is not a zlib payload")]
    ShareListCorrupt,
    #[error("shared file list exceeds {max} uncompressed bytes")]
    ShareListTooLarge { max: u64 },
}

/// One decoded server frame. `payload` does not include the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFrame {
    pub code: u32,
    pub payload: Vec<u8>,
}

/// Incremental frame reader. After [`ProtocolError::MessageTooLarge`] or
/// [`ProtocolError::MessageTooSmall`], the stream is desynchronized and the
/// connection must be closed.
#[derive(Debug)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    max_size: u32,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// Decoder capped at [`MAX_MESSAGE_16M`].
    pub fn new() -> Self {
        Self::with_max(MAX_MESSAGE_16M)
    }

    pub fn with_max(max_size: u32) -> Self {
        Self {
            buf: Vec::new(),
            max_size,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame, or `Ok(None)` when more bytes are required.
    pub fn pop(&mut self) -> Result<Option<ServerFrame>, ProtocolError> {
        if self.buf.len() < 8 {
            return Ok(None);
        }
        let size = read_u32(&self.buf[0..4]);
        let code = read_u32(&self.buf[4..8]);
        if size < 4 {
            return Err(ProtocolError::MessageTooSmall { size });
        }
        if size > self.max_size {
            return Err(ProtocolError::MessageTooLarge {
                size,
                max: self.max_size,
            });
        }
        let total = 4 + usize::try_from(size).map_err(|_| ProtocolError::IntegerOverflow)?;
        if self.buf.len() < total {
            return Ok(None);
        }
        let payload = self.buf[8..total].to_vec();
        self.buf.drain(..total);
        Ok(Some(ServerFrame { code, payload }))
    }
}

pub fn encode_frame(code: u32, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let body = 4usize
        .checked_add(payload.len())
        .ok_or(ProtocolError::IntegerOverflow)?;
    let size = u32::try_from(body).map_err(|_| ProtocolError::IntegerOverflow)?;
    let mut out = Vec::with_capacity(4 + body);
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&code.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn i32(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub(crate) fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn u64(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn string(&mut self, value: &str) -> Result<(), ProtocolError> {
        let bytes = value.as_bytes();
        let len = u32::try_from(bytes.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
        self.u32(len);
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn u8(&mut self) -> Result<u8, ProtocolError> {
        let bytes = self.bytes(1)?;
        Ok(bytes[0])
    }

    pub fn u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.bytes(4)?;
        Ok(read_u32(bytes))
    }

    pub fn i32(&mut self) -> Result<i32, ProtocolError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, ProtocolError> {
        let bytes = self.bytes(8)?;
        let mut buf = [0; 8];
        buf.copy_from_slice(bytes);
        Ok(u64::from_le_bytes(buf))
    }

    pub fn skip(&mut self, len: usize) -> Result<(), ProtocolError> {
        let _ = self.bytes(len)?;
        Ok(())
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    pub fn bool(&mut self) -> Result<bool, ProtocolError> {
        Ok(self.u8()? != 0)
    }

    /// Soulseek IPv4 addresses are a little-endian integer on the wire.
    pub fn ipv4(&mut self) -> Result<Ipv4Addr, ProtocolError> {
        let bytes = self.bytes(4)?;
        Ok(Ipv4Addr::new(bytes[3], bytes[2], bytes[1], bytes[0]))
    }

    pub fn string(&mut self) -> Result<String, ProtocolError> {
        let len = usize::try_from(self.u32()?).map_err(|_| ProtocolError::IntegerOverflow)?;
        let bytes = self.bytes(len)?;
        Ok(decode_text(bytes))
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ProtocolError::IntegerOverflow)?;
        if end > self.buf.len() {
            return Err(ProtocolError::Truncated {
                needed: end,
                available: self.buf.len(),
            });
        }
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    let mut buf = [0; 4];
    buf.copy_from_slice(&bytes[..4]);
    u32::from_le_bytes(buf)
}

fn decode_text(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().copied().map(char::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_partial_reads() {
        let frame = encode_frame(26, b"jazz").unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame[..3]);
        assert_eq!(decoder.pop().unwrap(), None);
        decoder.push(&frame[3..]);
        let decoded = decoder.pop().unwrap().unwrap();
        assert_eq!(decoded.code, 26);
        assert_eq!(decoded.payload, b"jazz");
        assert_eq!(decoder.pop().unwrap(), None);
    }

    #[test]
    fn two_frames_in_one_buffer() {
        let mut bytes = encode_frame(1, b"a").unwrap();
        bytes.extend(encode_frame(2, b"bc").unwrap());
        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);
        assert_eq!(decoder.pop().unwrap().unwrap().payload, b"a");
        assert_eq!(decoder.pop().unwrap().unwrap().payload, b"bc");
    }

    #[test]
    fn oversized_header_fails_before_the_body_arrives() {
        let mut decoder = FrameDecoder::with_max(16);
        let mut header = Vec::new();
        header.extend_from_slice(&32u32.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        decoder.push(&header);
        assert!(matches!(
            decoder.pop(),
            Err(ProtocolError::MessageTooLarge { size: 32, max: 16 })
        ));
    }

    #[test]
    fn undersized_header_fails() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&3u32.to_le_bytes());
        decoder.push(&1u32.to_le_bytes());
        assert!(matches!(
            decoder.pop(),
            Err(ProtocolError::MessageTooSmall { size: 3 })
        ));
    }

    #[test]
    fn ipv4_is_little_endian() {
        let mut reader = Reader::new(&[0x01, 0x00, 0x00, 0x7f]);
        assert_eq!(reader.ipv4().unwrap(), Ipv4Addr::new(127, 0, 0, 1));
    }

    #[test]
    fn legacy_bytes_decode_as_latin1() {
        let mut writer = Writer::new();
        writer.u32(1);
        let mut bytes = writer.finish();
        bytes.push(0xff);
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.string().unwrap(), "ÿ");
    }
}
