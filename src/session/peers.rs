//! In-flight peer sockets. Direct connect, then `ConnectToPeer` if that is refused.
//!
//! At most [`MAX_PEERS`] handshakes run at once. Established sockets count until they close.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::model::SearchMode;
use crate::protocol::{
    ConnType, PeerHandshake, PeerInitDecoder, ProtocolError, encode_peer_init,
    encode_pierce_firewall,
};

pub(crate) const MAX_PEERS: usize = 100;
pub(crate) const PEER_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) enum SessionCommand {
    ConnectPeer {
        username: String,
        conn_type: ConnType,
    },
    Search {
        mode: SearchMode,
        query: String,
        room: String,
        user: String,
    },
    Download {
        user: String,
        path: String,
        size: u64,
        folder: Option<String>,
        stage: bool,
    },
    /// A library upgrade search. Results stay off the interactive search list.
    QualitySearch {
        query: String,
    },
    /// A failed album search found no peer with a free upload slot.
    AlbumUnavailable {
        user: String,
        album: String,
    },
    /// A file-not-shared search found no peer with a free upload slot.
    FileUnavailable {
        user: String,
        path: String,
    },
    Cancel {
        direction: crate::model::Direction,
        user: String,
        path: String,
    },
    ClearFinished {
        downloads: bool,
        uploads: bool,
    },
    Browse {
        user: String,
    },
    Folder {
        user: String,
        directory: String,
    },
    SetLists {
        banned: Vec<String>,
        ignored: Vec<String>,
        prioritized: Vec<String>,
        ack: tokio::sync::oneshot::Sender<()>,
    },
    Say {
        room: String,
        text: String,
        ack: tokio::sync::oneshot::Sender<()>,
    },
    Join {
        room: String,
    },
    /// Asks the server for this user's status, stats, and interests.
    Inspect {
        user: String,
    },
    SetAway {
        away: bool,
        ack: tokio::sync::oneshot::Sender<()>,
    },
    Rescan {
        shares: crate::config::Shares,
    },
    Forget {
        paths: Vec<std::path::PathBuf>,
    },
    Relocate {
        moves: Vec<crate::library::Relocation>,
    },
    ApplyPrefs {
        prefs: crate::settings::LivePrefs,
    },
}

pub(crate) struct PeerBook {
    cap: usize,
    inflight: usize,
    next_token: u32,
    waiting: Vec<(String, ConnType, Instant)>,
    indirect: HashMap<u32, (String, ConnType, std::time::Instant)>,
}

impl PeerBook {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            cap,
            inflight: 0,
            next_token: 0,
            waiting: Vec::new(),
            indirect: HashMap::new(),
        }
    }

    pub(crate) fn try_reserve(&mut self) -> bool {
        if self.inflight >= self.cap {
            return false;
        }
        self.inflight += 1;
        true
    }

    pub(crate) fn release(&mut self) {
        self.inflight = self.inflight.saturating_sub(1);
    }

    pub(crate) fn connecting(&self, username: &str, conn_type: ConnType) -> bool {
        self.waiting
            .iter()
            .any(|(name, kind, _)| name == username && *kind == conn_type)
            || self
                .indirect
                .values()
                .any(|(name, kind, _)| name == username && *kind == conn_type)
    }

    pub(crate) fn push_wait(&mut self, username: String, conn_type: ConnType) {
        self.waiting.push((username, conn_type, Instant::now()));
    }

    pub(crate) fn take_wait(&mut self, username: &str) -> Option<ConnType> {
        let index = self
            .waiting
            .iter()
            .position(|(name, _, _)| name == username)?;
        Some(self.waiting.remove(index).1)
    }

    /// Address lookups that have been waiting longer than `limit`. Each one frees its slot.
    pub(crate) fn expire_waiting(&mut self, limit: Duration) -> Vec<(String, ConnType)> {
        let now = Instant::now();
        let mut kept = Vec::new();
        let mut stale = Vec::new();
        for (name, kind, started) in self.waiting.drain(..) {
            if now.duration_since(started) >= limit {
                stale.push((name, kind));
            } else {
                kept.push((name, kind, started));
            }
        }
        self.waiting = kept;
        for _ in &stale {
            self.release();
        }
        stale
    }

    pub(crate) fn begin_indirect(&mut self, username: String, conn_type: ConnType) -> u32 {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let token = self.next_token;
        self.indirect
            .insert(token, (username, conn_type, std::time::Instant::now()));
        token
    }

    pub(crate) fn take_indirect(&mut self, token: u32) -> Option<(String, ConnType)> {
        self.indirect
            .remove(&token)
            .map(|(username, conn_type, _)| (username, conn_type))
    }

    /// Indirect connects that have been waiting longer than `limit`.
    pub(crate) fn expire_indirect(
        &mut self,
        limit: std::time::Duration,
    ) -> Vec<(String, ConnType)> {
        let now = std::time::Instant::now();
        let stale: Vec<u32> = self
            .indirect
            .iter()
            .filter(|(_, (_, _, started))| now.duration_since(*started) >= limit)
            .map(|(token, _)| *token)
            .collect();
        let stale = stale
            .into_iter()
            .filter_map(|token| self.take_indirect(token))
            .collect::<Vec<_>>();
        for _ in &stale {
            self.release();
        }
        stale
    }
}

pub(crate) async fn dial_direct(
    addr: SocketAddr,
    our_username: &str,
    conn_type: ConnType,
) -> Result<TcpStream, ()> {
    let connect = tokio::time::timeout(PEER_CONNECT_TIMEOUT, TcpStream::connect(addr)).await;
    let Ok(Ok(mut stream)) = connect else {
        return Err(());
    };
    let frame = encode_peer_init(our_username, conn_type).map_err(|_| ())?;
    stream.write_all(&frame).await.map_err(|_| ())?;
    Ok(stream)
}

pub(crate) async fn dial_pierce(addr: SocketAddr, token: u32) -> Result<TcpStream, ()> {
    let connect = tokio::time::timeout(PEER_CONNECT_TIMEOUT, TcpStream::connect(addr)).await;
    let Ok(Ok(mut stream)) = connect else {
        return Err(());
    };
    let frame = encode_pierce_firewall(token).map_err(|_| ())?;
    stream.write_all(&frame).await.map_err(|_| ())?;
    Ok(stream)
}

pub(crate) async fn read_handshake(
    stream: &mut TcpStream,
) -> Result<(PeerHandshake, Vec<u8>), ProtocolError> {
    let mut decoder = PeerInitDecoder::new();
    let mut buf = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + PEER_CONNECT_TIMEOUT;
    loop {
        if let Some(message) = decoder.pop()? {
            return Ok((message, decoder.into_remainder()));
        }
        let Some(left) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            return Err(ProtocolError::Truncated {
                needed: 5,
                available: decoder.buffered(),
            });
        };
        let read = tokio::time::timeout(left, stream.read(&mut buf)).await;
        let count = match read {
            Ok(Ok(0)) | Err(_) => {
                return Err(ProtocolError::Truncated {
                    needed: 5,
                    available: decoder.buffered(),
                });
            }
            Ok(Err(_)) => {
                return Err(ProtocolError::Truncated {
                    needed: 5,
                    available: decoder.buffered(),
                });
            }
            Ok(Ok(count)) => count,
        };
        decoder.push(&buf[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_address_lookup_frees_the_only_slot() {
        let mut book = PeerBook::new(1);
        assert!(book.try_reserve());
        book.push_wait("bob".to_owned(), ConnType::Peer);
        assert!(!book.try_reserve());
        let stale = book.expire_waiting(Duration::ZERO);
        assert_eq!(stale, vec![("bob".to_owned(), ConnType::Peer)]);
        assert!(book.try_reserve());
    }
}
