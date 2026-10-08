//! Distributed-network frames.
//!
//! Nicotine+ 3.3.11 reads these the same way as peer init: `uint32 size`
//! (includes the type byte, excludes itself) and `uint8 type`. A size below 1
//! is shorter than 5 bytes and fails before the body. The cap is 16 KiB.
//! After the type byte, `DistribSearch` is an unknown `u32`, a username, a
//! token, and the query. `DistribBranchLevel` is an `i32`. `DistribBranchRoot`
//! is a username.

use super::codes::distributed;
use super::wire::{MAX_MESSAGE_16K, ProtocolError, Reader, Writer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistribMessage {
    Search {
        unknown: u32,
        username: String,
        token: u32,
        query: String,
    },
    BranchLevel {
        level: i32,
    },
    BranchRoot {
        username: String,
    },
    /// A type this client does not handle. The frame was consumed.
    Ignored,
}

pub fn encode_distrib_search(
    unknown: u32,
    username: &str,
    token: u32,
    query: &str,
) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(unknown);
    writer.string(username)?;
    writer.u32(token);
    writer.string(query)?;
    encode_distrib(distributed::DISTRIB_SEARCH, &writer.finish())
}

pub fn encode_distrib_branch_level(level: i32) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.i32(level);
    encode_distrib(distributed::DISTRIB_BRANCH_LEVEL, &writer.finish())
}

pub fn encode_distrib_branch_root(username: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(username)?;
    encode_distrib(distributed::DISTRIB_BRANCH_ROOT, &writer.finish())
}

fn code_byte(code: u32) -> Result<u8, ProtocolError> {
    u8::try_from(code).map_err(|_| ProtocolError::IntegerOverflow)
}

fn encode_distrib(code: u32, content: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let code = code_byte(code)?;
    let size = u32::try_from(1 + content.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
    if size > MAX_MESSAGE_16K {
        return Err(ProtocolError::MessageTooLarge {
            size,
            max: MAX_MESSAGE_16K,
        });
    }
    let mut out = Vec::with_capacity(4 + content.len() + 1);
    out.extend_from_slice(&size.to_le_bytes());
    out.push(code);
    out.extend_from_slice(content);
    Ok(out)
}

/// Incremental reader for distributed frames.
#[derive(Debug, Default)]
pub struct DistribDecoder {
    buf: Vec<u8>,
}

impl DistribDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn pop(&mut self) -> Result<Option<DistribMessage>, ProtocolError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let size = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        if size < 1 {
            return Err(ProtocolError::DistribTooSmall { size });
        }
        if size > MAX_MESSAGE_16K {
            return Err(ProtocolError::MessageTooLarge {
                size,
                max: MAX_MESSAGE_16K,
            });
        }
        let total = 4 + usize::try_from(size).map_err(|_| ProtocolError::IntegerOverflow)?;
        if self.buf.len() < total {
            return Ok(None);
        }
        let code = u32::from(self.buf[4]);
        let content = self.buf[5..total].to_vec();
        self.buf.drain(..total);
        match code {
            distributed::DISTRIB_SEARCH => {
                let mut reader = Reader::new(&content);
                Ok(Some(DistribMessage::Search {
                    unknown: reader.u32()?,
                    username: reader.string()?,
                    token: reader.u32()?,
                    query: reader.string()?,
                }))
            }
            distributed::DISTRIB_BRANCH_LEVEL => {
                let mut reader = Reader::new(&content);
                Ok(Some(DistribMessage::BranchLevel {
                    level: reader.i32()?,
                }))
            }
            distributed::DISTRIB_BRANCH_ROOT => {
                let mut reader = Reader::new(&content);
                Ok(Some(DistribMessage::BranchRoot {
                    username: reader.string()?,
                }))
            }
            _ => Ok(Some(DistribMessage::Ignored)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn distrib_search_matches_the_nicotine_layout() {
        let frame = encode_distrib_search(0, "bob", 7, "jazz").unwrap();
        assert_eq!(
            frame,
            hex("18000000030000000003000000626f6207000000040000006a617a7a")
        );
        let mut decoder = DistribDecoder::new();
        decoder.push(&frame);
        assert_eq!(
            decoder.pop().unwrap().unwrap(),
            DistribMessage::Search {
                unknown: 0,
                username: "bob".to_owned(),
                token: 7,
                query: "jazz".to_owned(),
            }
        );
    }

    #[test]
    fn branch_level_and_root_match_the_nicotine_layout() {
        assert_eq!(
            encode_distrib_branch_level(2).unwrap(),
            hex("050000000402000000")
        );
        assert_eq!(
            encode_distrib_branch_root("alice").unwrap(),
            hex("0a0000000505000000616c696365")
        );
    }

    #[test]
    fn distrib_shorter_than_five_bytes_fails() {
        let mut decoder = DistribDecoder::new();
        decoder.push(&0u32.to_le_bytes());
        assert_eq!(
            decoder.pop().unwrap_err(),
            ProtocolError::DistribTooSmall { size: 0 }
        );
    }
}
