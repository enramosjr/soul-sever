//! Search queries, wishlist rotation, and answers from the share index.

use std::collections::{HashMap, HashSet};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::config::{Buddy, Config};

use crate::model::{SearchHit, SearchMode};
use crate::protocol::peer::{
    FILE_ATTRIBUTE_BIT_DEPTH, FILE_ATTRIBUTE_BITRATE, FILE_ATTRIBUTE_DURATION,
    FILE_ATTRIBUTE_SAMPLE_RATE, FILE_SEARCH_RESPONSE, SharedFileListResponse, SharedFolder,
};
use crate::protocol::{
    ClientMessage, DistribDecoder, DistribMessage, FileSearchRequest, FileSearchResponse,
    FrameDecoder, SearchResultFile, ServerFrame, encode_frame, sanitize_search_query,
};
use crate::shares::{IndexedFile, ShareIndex};

/// A search the terminal asked the session to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    pub mode: SearchMode,
    pub query: String,
    pub room: String,
    pub user: String,
}

/// Files whose names contain every required token. A query shorter than
/// `min_chars` matches nothing. At most `max_results` files are returned.
pub fn matching_files<'a>(
    index: &'a ShareIndex,
    query: &str,
    min_chars: u32,
    max_results: usize,
) -> Vec<&'a IndexedFile> {
    let query = sanitize_search_query(query);
    if query.chars().count() < usize::try_from(min_chars).unwrap_or(usize::MAX) {
        return Vec::new();
    }
    let mut required = Vec::new();
    let mut excluded = Vec::new();
    for token in query.split_whitespace() {
        if let Some(rest) = token.strip_prefix('-') {
            if !rest.is_empty() {
                excluded.push(rest.to_lowercase());
            }
        } else {
            required.push(token.to_lowercase());
        }
    }
    if required.is_empty() {
        return Vec::new();
    }
    let mut ids: Option<HashSet<u32>> = None;
    for token in &required {
        let hits: HashSet<u32> = index
            .lookup(token)
            .into_iter()
            .map(|file| file.id)
            .collect();
        ids = Some(match ids {
            None => hits,
            Some(current) => current.intersection(&hits).copied().collect(),
        });
    }
    let Some(ids) = ids else {
        return Vec::new();
    };
    index
        .files()
        .iter()
        .filter(|file| ids.contains(&file.id))
        .filter(|file| {
            let words: HashSet<String> = file
                .name
                .split(|character: char| !character.is_alphanumeric())
                .filter(|part| !part.is_empty())
                .map(|part| part.to_lowercase())
                .collect();
            excluded.iter().all(|token| !words.contains(token))
        })
        .take(max_results)
        .collect()
}

fn response_frame(
    index: &ShareIndex,
    our_username: &str,
    token: u32,
    query: &str,
    min_chars: u32,
    max_results: usize,
) -> Option<(Vec<u8>, usize)> {
    let files = matching_files(index, query, min_chars, max_results);
    if files.is_empty() {
        return None;
    }
    let response = FileSearchResponse {
        username: our_username.to_owned(),
        token,
        files: files
            .iter()
            .map(|file| SearchResultFile {
                path: format!("{}\\{}", file.directory, file.name),
                size: file.size,
                attributes: file.attributes.clone(),
            })
            .collect(),
        free_slot: true,
        upload_speed: 0,
        queue: 0,
        private_files: Vec::new(),
    };
    let results = response.files.len();
    let payload = response.encode().ok()?;
    encode_frame(FILE_SEARCH_RESPONSE, &payload)
        .ok()
        .map(|frame| (frame, results))
}

pub fn hits_from(response: &FileSearchResponse) -> Vec<SearchHit> {
    response
        .files
        .iter()
        .map(|file| SearchHit {
            user: response.username.clone(),
            path: file.path.clone(),
            size: file.size,
            bitrate: attribute(&file.attributes, FILE_ATTRIBUTE_BITRATE),
            duration: attribute(&file.attributes, FILE_ATTRIBUTE_DURATION),
            bit_depth: attribute(&file.attributes, FILE_ATTRIBUTE_BIT_DEPTH),
            sample_rate: attribute(&file.attributes, FILE_ATTRIBUTE_SAMPLE_RATE),
            queue: response.queue,
            free_slot: response.free_slot,
            upload_speed: response.upload_speed,
            country: String::new(),
        })
        .collect()
}

fn attribute(attributes: &[(u32, u32)], code: u32) -> Option<u32> {
    attributes
        .iter()
        .find(|(found, _)| *found == code)
        .map(|(_, value)| *value)
}

pub fn outbound(
    request: &SearchRequest,
    token: u32,
    buddies: &[String],
) -> Result<Vec<ClientMessage>, &'static str> {
    let query = sanitize_search_query(&request.query);
    if query.is_empty() {
        return Err("search query is empty");
    }
    match request.mode {
        SearchMode::Global => Ok(vec![ClientMessage::FileSearch(FileSearchRequest {
            token,
            query,
        })]),
        SearchMode::Room => {
            if request.room.is_empty() {
                return Err("no room selected");
            }
            Ok(vec![ClientMessage::RoomSearch {
                room: request.room.clone(),
                token,
                query,
            }])
        }
        SearchMode::User => {
            if request.user.is_empty() {
                return Err("no user selected");
            }
            Ok(vec![ClientMessage::UserSearch {
                username: request.user.clone(),
                token,
                query,
            }])
        }
        SearchMode::Buddy => {
            if buddies.is_empty() {
                return Err("no buddies to search");
            }
            Ok(buddies
                .iter()
                .map(|username| ClientMessage::UserSearch {
                    username: username.clone(),
                    token,
                    query: query.clone(),
                })
                .collect())
        }
        SearchMode::Wishlist => Err("wishlist searches wait for the server interval"),
    }
}

pub fn wishlist_message(query: &str, token: u32) -> Option<ClientMessage> {
    let query = sanitize_search_query(query);
    if query.is_empty() {
        return None;
    }
    Some(ClientMessage::WishlistSearch(FileSearchRequest {
        token,
        query,
    }))
}

pub enum PeerIn {
    Frame {
        username: String,
        frame: ServerFrame,
    },
    /// The peer socket closed. `generation` is the connection that died.
    Closed { username: String, generation: u64 },
}

pub async fn read_peer_frames(
    stream: OwnedReadHalf,
    leftover: Vec<u8>,
    username: String,
    generation: u64,
    tx: mpsc::Sender<PeerIn>,
) {
    read_peer_frames_inner(stream, leftover, &username, &tx).await;
    let _ = tx
        .send(PeerIn::Closed {
            username,
            generation,
        })
        .await;
}

async fn read_peer_frames_inner(
    mut stream: OwnedReadHalf,
    leftover: Vec<u8>,
    username: &str,
    tx: &mpsc::Sender<PeerIn>,
) {
    let mut decoder = FrameDecoder::new();
    decoder.push(&leftover);
    let mut buf = [0u8; 8192];
    loop {
        loop {
            match decoder.pop() {
                Ok(Some(frame)) => {
                    if tx
                        .send(PeerIn::Frame {
                            username: username.to_owned(),
                            frame,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        let count = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(count) => count,
        };
        decoder.push(&buf[..count]);
    }
}

pub async fn read_distrib(
    mut stream: OwnedReadHalf,
    leftover: Vec<u8>,
    from_parent: bool,
    tx: mpsc::Sender<DistribIn>,
) {
    let mut decoder = DistribDecoder::new();
    decoder.push(&leftover);
    let mut buf = [0u8; 8192];
    'read: loop {
        loop {
            match decoder.pop() {
                Ok(Some(DistribMessage::Ignored)) => {}
                Ok(Some(message)) => {
                    if tx
                        .send(DistribIn {
                            from_parent,
                            message: Some(message),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => break,
                Err(_) => break 'read,
            }
        }
        let count = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break 'read,
            Ok(count) => count,
        };
        decoder.push(&buf[..count]);
    }
    let _ = tx
        .send(DistribIn {
            from_parent,
            message: None,
        })
        .await;
}

pub struct DistribIn {
    pub from_parent: bool,
    pub message: Option<DistribMessage>,
}

pub(crate) struct SearchState {
    next_token: u32,
    /// The file search the results list is showing. A new search drops this token.
    interactive: Option<u32>,
    /// A library upgrade search. It stays open when the interactive search changes.
    probe: Option<u32>,
    open: HashSet<u32>,
    pending: HashMap<String, Vec<u8>>,
    wishlist: Vec<String>,
    wishlist_at: usize,
    index: ShareIndex,
    min_chars: u32,
    max_results: usize,
    buddies: Vec<String>,
    buddy_records: Vec<Buddy>,
    pub parent: Option<String>,
    pub pending_parent: Option<String>,
    pub children: Vec<OwnedWriteHalf>,
    pub child_count: usize,
}

impl SearchState {
    pub fn new(config: &Config, index: ShareIndex) -> Self {
        Self {
            next_token: 0,
            interactive: None,
            probe: None,
            open: HashSet::new(),
            pending: HashMap::new(),
            wishlist: config.wishlist.clone(),
            wishlist_at: 0,
            min_chars: config.min_search_chars,
            max_results: usize::try_from(config.max_results).unwrap_or(usize::MAX),
            buddies: config
                .buddies
                .iter()
                .map(|buddy| buddy.name.clone())
                .collect(),
            buddy_records: config.buddies.clone(),
            index,
            parent: None,
            pending_parent: None,
            children: Vec::new(),
            child_count: 0,
        }
    }

    pub fn set_index(&mut self, index: ShareIndex) {
        self.index = index;
    }

    pub fn index(&self) -> &ShareIndex {
        &self.index
    }

    /// Indexes a filed download and returns a public share root when one was added.
    pub fn store_download(
        &mut self,
        shares: &mut crate::config::Shares,
        root: &std::path::Path,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        self.index.store_moved(shares, root, from, to)
    }

    /// Drops deleted library files from the share index.
    pub fn forget(&mut self, paths: &[std::path::PathBuf]) {
        self.index.forget(paths);
    }

    /// Public folder count, then public file count. Buddy and trusted stay out.
    pub fn public_counts(&self) -> (u32, u32) {
        let public = self.index.counts()[0];
        (public.folders, public.files)
    }

    pub fn share_list(&self, username: &str) -> SharedFileListResponse {
        self.index.list_for(username, &self.buddy_records)
    }

    /// Folders named `directory` that `username` is allowed to browse.
    pub fn folder_for(&self, username: &str, directory: &str) -> Vec<SharedFolder> {
        let want = directory.replace('/', "\\");
        self.share_list(username)
            .list
            .into_iter()
            .filter(|folder| folder.directory.replace('/', "\\") == want)
            .collect()
    }

    pub fn apply_prefs(&mut self, prefs: &crate::settings::LivePrefs) {
        self.wishlist.clone_from(&prefs.wishlist);
        self.wishlist_at = 0;
        self.min_chars = prefs.min_search_chars;
        self.max_results = usize::try_from(prefs.max_results).unwrap_or(usize::MAX);
    }

    pub fn next_token(&mut self) -> u32 {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.open.insert(self.next_token);
        self.next_token
    }

    /// Opens a token for the results list and closes the previous one.
    pub fn begin_search(&mut self) -> u32 {
        self.cancel_search();
        let token = self.next_token();
        self.interactive = Some(token);
        token
    }

    pub fn cancel_search(&mut self) {
        if let Some(token) = self.interactive.take() {
            self.open.remove(&token);
        }
    }

    /// Opens a token that does not replace the interactive search.
    pub fn begin_probe(&mut self) -> u32 {
        self.end_probe();
        let token = self.next_token();
        self.probe = Some(token);
        token
    }

    pub fn end_probe(&mut self) {
        if let Some(token) = self.probe.take() {
            self.open.remove(&token);
        }
    }

    pub fn is_probe(&self, token: u32) -> bool {
        self.probe == Some(token)
    }

    pub fn accepts(&self, token: u32) -> bool {
        self.open.contains(&token)
    }

    pub fn queue_reply(&mut self, username: String, frame: Vec<u8>) {
        self.pending.insert(username, frame);
    }

    pub fn take_reply(&mut self, username: &str) -> Option<Vec<u8>> {
        self.pending.remove(username)
    }

    /// The peer frame, and how many files it lists.
    pub fn answer(&self, token: u32, query: &str, our_username: &str) -> Option<(Vec<u8>, usize)> {
        response_frame(
            &self.index,
            our_username,
            token,
            query,
            self.min_chars,
            self.max_results,
        )
    }

    pub fn rotate_wishlist(&mut self) -> Option<String> {
        if self.wishlist.is_empty() {
            return None;
        }
        let query = self.wishlist[self.wishlist_at].clone();
        self.wishlist_at = (self.wishlist_at + 1) % self.wishlist.len();
        Some(query)
    }

    pub fn buddies(&self) -> &[String] {
        &self.buddies
    }
}

pub async fn relay_search(children: &mut [OwnedWriteHalf], message: &DistribMessage) {
    let DistribMessage::Search {
        unknown,
        username,
        token,
        query,
    } = message
    else {
        return;
    };
    let Ok(frame) = crate::protocol::encode_distrib_search(*unknown, username, *token, query)
    else {
        return;
    };
    for child in children {
        let _ = child.write_all(&frame).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Shares;
    use std::fs;

    #[tokio::test]
    async fn a_shared_token_matches_and_one_character_does_not() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-search-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let public = root.join("public");
        fs::create_dir_all(&public).unwrap();
        fs::write(public.join("notes.txt"), b"hello").unwrap();
        let shares = Shares {
            public: vec![public],
            ..Shares::default()
        };
        let index = crate::shares::rescan(&shares).await.unwrap();
        assert!(matching_files(&index, "n", 3, 300).is_empty());
        let hits = matching_files(&index, "notes", 3, 300);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "notes.txt");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_library_relocation_replaces_the_share_path() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-relocate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let from = root
            .join("Ella Langley with Riley Green")
            .join("Hungover")
            .join("01 You.wav");
        fs::create_dir_all(from.parent().unwrap()).unwrap();
        fs::write(&from, b"song").unwrap();
        let mut shares = Shares {
            public: vec![root.clone()],
            ..Shares::default()
        };
        let index = crate::shares::rescan(&shares).await.unwrap();
        let before = matching_files(&index, "You", 3, 10);
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].path, from);

        let to = root
            .join("Ella Langley")
            .join("Hungover")
            .join("01 You.wav");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::rename(&from, &to).unwrap();
        let mut state = SearchState::new(&crate::config::Config::default(), index);
        state.store_download(&mut shares, &root, &from, &to);
        let after = matching_files(state.index(), "You", 3, 10);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].path, to);
        assert!(after[0].directory.contains("Ella Langley"));
        assert!(!after[0].directory.contains("with Riley"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_search_closes_the_previous_token() {
        let mut state = SearchState::new(&crate::config::Config::default(), ShareIndex::empty());
        let first = state.begin_search();
        let second = state.begin_search();
        let wishlist = state.next_token();
        assert_ne!(first, second);
        assert!(!state.accepts(first));
        assert!(state.accepts(second));
        assert!(state.accepts(wishlist));
        state.cancel_search();
        assert!(!state.accepts(second));
        assert!(state.accepts(wishlist));
    }

    #[test]
    fn wishlist_enter_does_not_build_a_server_message() {
        let request = SearchRequest {
            mode: SearchMode::Wishlist,
            query: "jazz".to_owned(),
            room: String::new(),
            user: String::new(),
        };
        assert!(outbound(&request, 1, &[]).is_err());
        assert!(wishlist_message("jazz", 1).is_some());
    }
}
