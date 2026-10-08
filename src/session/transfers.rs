//! Download and upload queue.
//!
//! A download sends `QueueUpload`, accepts the peer's `TransferRequest`, then
//! reads the file on a later `F` connection. An upload occupies one of
//! `upload_slots` until the file socket finishes.

use std::collections::HashSet;
use std::fs::{self, OpenOptions as StdOpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config::{Config, QueueMode};
use crate::model::{Direction, Transfer, TransferState};
use crate::protocol::peer::{
    DIRECTION_DOWNLOAD, DIRECTION_UPLOAD, PLACE_IN_QUEUE_REQUEST, PLACE_IN_QUEUE_RESPONSE,
    QUEUE_UPLOAD, REJECT_BANNED, REJECT_CANCELLED, REJECT_FILE_NOT_SHARED, REJECT_FILTERED,
    REJECT_PENDING_SHUTDOWN, REJECT_QUEUED, REJECT_TOO_MANY_FILES, REJECT_TOO_MANY_MEGABYTES,
    TRANSFER_REQUEST, TRANSFER_RESPONSE, TransferRequest, TransferResponse, UPLOAD_DENIED,
    UPLOAD_FAILED, decode_place_in_queue, decode_place_request, decode_queue_upload,
    decode_transfer_request, decode_transfer_response, decode_upload_denied, decode_upload_failed,
    encode_file_offset, encode_file_token, encode_place_in_queue, encode_place_request,
    encode_queue_upload, encode_transfer_request, encode_transfer_response, encode_upload_denied,
    encode_upload_failed,
};
use crate::shares::{IndexedFile, ShareIndex};

/// A file the terminal asked the session to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub user: String,
    pub path: String,
    pub size: u64,
    /// Album directory from a folder download. The finished file keeps that folder's own name.
    pub folder: Option<String>,
    /// Land in `incomplete/quality` and do not file the song until it is verified.
    pub stage: bool,
}

/// One shared file the upload queue can send.
#[derive(Debug, Clone)]
pub struct Shared {
    pub virtual_path: String,
    pub disk_path: PathBuf,
    pub size: u64,
}

pub fn shared_files(index: &ShareIndex) -> Vec<Shared> {
    index
        .files()
        .iter()
        .map(|file| Shared {
            virtual_path: virtual_path(file),
            disk_path: file.path.clone(),
            size: file.size,
        })
        .collect()
}

fn virtual_path(file: &IndexedFile) -> String {
    format!("{}\\{}", file.directory, file.name)
}

/// How long a `limit_kib` per-second cap should pause after writing `bytes`.
pub fn chunk_delay(bytes: usize, limit_kib: u32) -> Duration {
    let per_second = u64::from(limit_kib).saturating_mul(1024);
    if per_second == 0 {
        return Duration::ZERO;
    }
    Duration::from_micros(
        u64::try_from(bytes)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_000_000)
            / per_second,
    )
}

#[derive(Debug)]
pub struct Progress {
    pub direction: Direction,
    pub user: String,
    pub path: String,
    pub done: u64,
    /// Bytes per second over the interval since the previous report. Zero when the transfer is not running.
    pub speed: u64,
    pub state: TransferState,
    /// Where a finished download was filed. Absent until the file is complete.
    pub placed: Option<PathBuf>,
    /// Set when that finished path differs from the Soulseek destination.
    pub moved: Option<MovedFile>,
}

/// A download that was filed somewhere other than its Soulseek destination.
#[derive(Debug, Clone)]
pub struct MovedFile {
    pub root: PathBuf,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Non-audio files removed from the music folder after this file was filed.
    pub swept: Vec<PathBuf>,
}

pub struct Book {
    downloads: Vec<Item>,
    uploads: Vec<Item>,
    shared: Vec<Shared>,
    slots: usize,
    queue_mode: QueueMode,
    file_limit: u32,
    byte_limit: u64,
    banned: HashSet<String>,
    ignored: HashSet<String>,
    prioritized: HashSet<String>,
    privileged: HashSet<String>,
    incomplete_dir: PathBuf,
    download_dir: PathBuf,
    upload_limit_kib: Option<u32>,
    download_limit_kib: Option<u32>,
    next_token: u32,
    next_seq: u64,
    last_upload_user: Option<String>,
    queue_path: Option<PathBuf>,
}

struct Item {
    user: String,
    path: String,
    disk: PathBuf,
    dest: PathBuf,
    size: u64,
    done: u64,
    speed: u64,
    token: u32,
    state: TransferState,
    place: Option<u32>,
    asked: bool,
    started: bool,
    filing: bool,
    seq: u64,
    reason: String,
    /// Album directory from a folder download. Absent for a single file.
    folder: Option<String>,
    /// A quality replacement. The file stays under `incomplete/quality` until it is checked.
    stage: bool,
}

impl Book {
    pub fn new(config: &Config, shared: Vec<Shared>) -> Self {
        Self {
            downloads: Vec::new(),
            uploads: Vec::new(),
            shared,
            slots: usize::from(config.upload_slots),
            queue_mode: config.queue_mode,
            file_limit: config.queue_file_limit,
            byte_limit: u64::from(config.queue_megabytes).saturating_mul(1024 * 1024),
            banned: config.banned.iter().cloned().collect(),
            ignored: config.ignored.iter().cloned().collect(),
            prioritized: config
                .buddies
                .iter()
                .filter(|buddy| buddy.prioritized)
                .map(|buddy| buddy.name.clone())
                .collect(),
            privileged: HashSet::new(),
            incomplete_dir: PathBuf::from(&config.incomplete_dir),
            download_dir: PathBuf::from(&config.download_dir),
            upload_limit_kib: config.upload_limit_kib,
            download_limit_kib: config.download_limit_kib,
            next_token: 0,
            next_seq: 0,
            last_upload_user: None,
            queue_path: None,
        }
    }

    /// Loads `path` and keeps writing the queue there.
    pub fn use_queue(&mut self, path: PathBuf) {
        self.load_queue(&path);
        self.queue_path = Some(path);
    }

    pub fn snapshot(&self) -> Vec<Transfer> {
        let mut rows = Vec::new();
        rows.extend(
            self.downloads
                .iter()
                .map(|item| item.view(Direction::Download)),
        );
        rows.extend(self.uploads.iter().map(|item| item.view(Direction::Upload)));
        rows
    }

    /// Usernames with a download that should be asked for again.
    pub fn queued_download_users(&self) -> Vec<String> {
        let mut users = Vec::new();
        for item in &self.downloads {
            if item.state == TransferState::Queued && !users.contains(&item.user) {
                users.push(item.user.clone());
            }
        }
        users
    }

    /// Drops finished rows. `downloads` and `uploads` choose which lists.
    pub fn clear_finished(&mut self, downloads: bool, uploads: bool) {
        if downloads {
            self.downloads
                .retain(|item| item.state != TransferState::Finished);
        }
        if uploads {
            self.uploads
                .retain(|item| item.state != TransferState::Finished);
        }
        self.persist();
    }

    pub fn set_privileged(&mut self, user: &str, privileged: bool) {
        if privileged {
            self.privileged.insert(user.to_owned());
        } else {
            self.privileged.remove(user);
        }
    }

    pub fn set_shared(&mut self, shared: Vec<Shared>) {
        self.shared = shared;
    }

    pub fn apply_prefs(&mut self, prefs: &crate::settings::LivePrefs) {
        self.slots = usize::from(prefs.upload_slots);
        self.queue_mode = prefs.queue_mode;
        self.upload_limit_kib = prefs.upload_limit_kib;
        self.download_limit_kib = prefs.download_limit_kib;
        self.incomplete_dir = PathBuf::from(&prefs.incomplete_dir);
        self.download_dir = PathBuf::from(&prefs.download_dir);
        self.file_limit = prefs.queue_file_limit;
        self.byte_limit = u64::from(prefs.queue_megabytes).saturating_mul(1024 * 1024);
    }

    pub fn set_blocks(&mut self, banned: &[String], ignored: &[String], prioritized: &[String]) {
        self.banned = banned.iter().cloned().collect();
        self.ignored = ignored.iter().cloned().collect();
        self.prioritized = prioritized.iter().cloned().collect();
    }

    pub fn blocks(&self, user: &str) -> bool {
        self.banned.contains(user) || self.ignored.contains(user)
    }

    pub fn upload_limit(&self) -> Option<u32> {
        self.upload_limit_kib
    }

    pub fn download_limit(&self) -> Option<u32> {
        self.download_limit_kib
    }

    /// Queue a download. The caller sends `QueueUpload` once a peer socket exists.
    pub fn enqueue_download(&mut self, request: &DownloadRequest) -> Result<Transfer, String> {
        if self.incomplete_dir.as_os_str().is_empty() {
            return Err("incomplete folder is unset".to_owned());
        }
        if self.download_dir.as_os_str().is_empty() {
            return Err("download folder is unset".to_owned());
        }
        std::fs::create_dir_all(&self.incomplete_dir).map_err(|err| err.to_string())?;
        std::fs::create_dir_all(&self.download_dir).map_err(|err| err.to_string())?;
        let disk = incomplete_file(&self.incomplete_dir, &request.user, &request.path);
        let done = std::fs::metadata(&disk).map(|meta| meta.len()).unwrap_or(0);
        let dest = if request.stage {
            quality_stage(&self.incomplete_dir, &request.path)
        } else {
            download_dest(&self.download_dir, request.folder.as_deref(), &request.path)
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let updated = if let Some(item) = self
            .downloads
            .iter_mut()
            .find(|item| item.user == request.user && item.path == request.path)
        {
            item.size = request.size;
            item.done = done.min(request.size);
            item.speed = 0;
            item.disk = disk.clone();
            item.dest = dest.clone();
            item.stage = request.stage;
            item.asked = false;
            item.reason.clear();
            if request.folder.is_some() {
                item.folder.clone_from(&request.folder);
            }
            item.state = if item.done >= item.size && item.size > 0 {
                TransferState::Finished
            } else {
                TransferState::Queued
            };
            Some(item.view(Direction::Download))
        } else {
            None
        };
        if let Some(view) = updated {
            self.persist();
            return Ok(view);
        }
        let seq = self.alloc_seq();
        let item = Item {
            user: request.user.clone(),
            path: request.path.clone(),
            disk,
            dest,
            size: request.size,
            done: done.min(request.size),
            speed: 0,
            token: 0,
            state: TransferState::Queued,
            place: None,
            asked: false,
            started: false,
            filing: false,
            seq,
            reason: String::new(),
            folder: request.folder.clone(),
            stage: request.stage,
        };
        let view = item.view(Direction::Download);
        self.downloads.push(item);
        self.persist();
        Ok(view)
    }

    /// Removes one transfer. A waiting upload is told `Cancelled`. A download the peer still offers is refused later.
    pub fn cancel(&mut self, direction: Direction, user: &str, path: &str) -> Vec<Out> {
        let items = match direction {
            Direction::Download => &mut self.downloads,
            Direction::Upload => &mut self.uploads,
        };
        let Some(index) = items
            .iter()
            .position(|item| item.user == user && item.path == path)
        else {
            return Vec::new();
        };
        let item = items.remove(index);
        self.persist();
        if direction == Direction::Upload && !item.started {
            let mut out = deny(&item.user, &item.path, REJECT_CANCELLED);
            out.extend(self.pump());
            return out;
        }
        if direction == Direction::Upload {
            return self.pump();
        }
        Vec::new()
    }

    /// `QueueUpload` frames for this peer. Marks them asked.
    pub fn take_queue_uploads(&mut self, user: &str) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for item in &mut self.downloads {
            if item.user != user || item.asked || item.state != TransferState::Queued {
                continue;
            }
            if let Ok(frame) = encode_queue_upload(&item.path) {
                item.asked = true;
                frames.push(frame);
            }
        }
        frames
    }

    /// The peer socket died before these downloads left the queue. Ask again.
    pub fn requeue_asks(&mut self, user: &str) {
        for item in &mut self.downloads {
            if item.user == user && item.state == TransferState::Queued {
                item.asked = false;
                item.place = None;
            }
        }
    }

    pub fn has_queued(&self, user: &str) -> bool {
        self.downloads
            .iter()
            .any(|item| item.user == user && item.state == TransferState::Queued)
    }

    /// `PlaceInQueueRequest` for downloads we already asked this peer to queue.
    pub fn place_requests(&self, user: &str) -> Vec<Vec<u8>> {
        self.downloads
            .iter()
            .filter(|item| {
                item.user == user
                    && item.asked
                    && item.place.is_none()
                    && item.state == TransferState::Queued
            })
            .filter_map(|item| encode_place_request(&item.path).ok())
            .collect()
    }

    /// Connection closed and connection timeout go back to the queue. Bytes already
    /// stored stay, so the next offer resumes at that offset.
    pub fn retry_connection_failures(&mut self) -> (Vec<Out>, Vec<String>) {
        let mut out = Vec::new();
        let mut users = Vec::new();
        for item in &mut self.downloads {
            if !matches!(
                item.state,
                TransferState::ConnectionClosed | TransferState::ConnectionTimeout
            ) {
                continue;
            }
            item.state = TransferState::Queued;
            item.asked = false;
            item.place = None;
            item.speed = 0;
            item.token = 0;
            if !users.contains(&item.user) {
                users.push(item.user.clone());
            }
            out.push(Out::Event(item.view(Direction::Download)));
        }
        if !out.is_empty() {
            self.persist();
        }
        (out, users)
    }

    /// The peer cannot take these queued downloads. The row keeps that status.
    pub fn fail_queued(&mut self, user: &str, state: TransferState) -> Vec<Out> {
        let mut out = Vec::new();
        for item in &mut self.downloads {
            if item.user != user || item.state != TransferState::Queued {
                continue;
            }
            item.state = state;
            item.place = None;
            out.push(Out::Event(item.view(Direction::Download)));
        }
        if state == TransferState::ConnectionClosed {
            out.extend(self.claim_failed_albums());
        }
        if !out.is_empty() {
            self.persist();
        }
        out
    }

    /// An album whose every file is `Connection closed` becomes `Failed`.
    /// Partial files are deleted, and the album name is returned for a new search.
    fn claim_failed_albums(&mut self) -> Vec<Out> {
        let mut keys = Vec::new();
        for item in &self.downloads {
            let Some(folder) = item.folder.clone() else {
                continue;
            };
            if folder.is_empty() {
                continue;
            }
            let key = (item.user.clone(), folder);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        let mut out = Vec::new();
        for (user, folder) in keys {
            let indexes: Vec<usize> = self
                .downloads
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    item.user == user && item.folder.as_deref() == Some(folder.as_str())
                })
                .map(|(index, _)| index)
                .collect();
            if indexes.is_empty()
                || !indexes
                    .iter()
                    .all(|&index| self.downloads[index].state == TransferState::ConnectionClosed)
            {
                continue;
            }
            let album = folder
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("")
                .trim()
                .to_owned();
            if album.is_empty() {
                continue;
            }
            for index in indexes {
                let item = &mut self.downloads[index];
                item.state = TransferState::Failed;
                item.speed = 0;
                item.place = None;
                item.done = 0;
                let disk = item.disk.clone();
                let dest = item.dest.clone();
                remove_partial(&disk, &dest, &self.download_dir);
                out.push(Out::Event(item.view(Direction::Download)));
            }
            out.push(Out::AlbumFailed { user, album });
        }
        if !out.is_empty() {
            self.persist();
        }
        out
    }

    /// The replacement search found no free slot for this file. That `Failed` row says so.
    pub fn mark_file_unavailable(&mut self, user: &str, path: &str) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(item) = self
            .downloads
            .iter_mut()
            .find(|item| item.user == user && same_path(&item.path, path))
            && matches!(
                item.state,
                TransferState::Failed | TransferState::LastTryFailed
            )
        {
            item.state = TransferState::Failed;
            item.reason = "Unavailable".to_owned();
            out.push(Out::Event(item.view(Direction::Download)));
        }
        if !out.is_empty() {
            self.persist();
        }
        out
    }

    /// The replacement search found no free slot. Those `Failed` rows say so.
    pub fn mark_unavailable(&mut self, user: &str, album: &str) -> Vec<Out> {
        let mut out = Vec::new();
        for item in &mut self.downloads {
            if item.state != TransferState::Failed || !item.user.eq_ignore_ascii_case(user) {
                continue;
            }
            let Some(folder) = item.folder.as_deref() else {
                continue;
            };
            let name = folder.rsplit(['\\', '/']).next().unwrap_or("").trim();
            if !name.eq_ignore_ascii_case(album) {
                continue;
            }
            item.reason = "Unavailable".to_owned();
            out.push(Out::Event(item.view(Direction::Download)));
        }
        if !out.is_empty() {
            self.persist();
        }
        out
    }

    /// Peer messages to write back, and usernames that need an `F` connection.
    pub fn on_peer(&mut self, user: &str, code: u32, payload: &[u8]) -> Vec<Out> {
        let mut out = Vec::new();
        if code == QUEUE_UPLOAD {
            if let Ok(message) = decode_queue_upload(payload) {
                out.extend(self.enqueue_upload(user, &message.file));
            }
        } else if code == TRANSFER_REQUEST {
            if let Ok(message) = decode_transfer_request(payload) {
                out.extend(self.on_transfer_request(user, &message));
            }
        } else if code == TRANSFER_RESPONSE {
            if let Ok(message) = decode_transfer_response(payload) {
                out.extend(self.on_transfer_response(user, &message));
            }
        } else if code == UPLOAD_DENIED {
            if let Ok(message) = decode_upload_denied(payload) {
                let (state, reason) = refusal(&message.reason);
                self.mark_download(user, &message.file, 0, state, &reason, &mut out);
            }
        } else if code == UPLOAD_FAILED {
            if let Ok(message) = decode_upload_failed(payload) {
                self.mark_download(
                    user,
                    &message.file,
                    0,
                    TransferState::ConnectionClosed,
                    "",
                    &mut out,
                );
            }
        } else if code == PLACE_IN_QUEUE_RESPONSE {
            if let Ok(message) = decode_place_in_queue(payload) {
                self.note_place(user, &message.file, message.place, &mut out);
            }
        } else if code == PLACE_IN_QUEUE_REQUEST
            && let Ok(message) = decode_place_request(payload)
        {
            let place = self.place_of(user, &message.file);
            if let Ok(frame) = encode_place_in_queue(&message.file, place) {
                out.push(Out::Write {
                    user: user.to_owned(),
                    frame,
                });
            }
        }
        out
    }

    pub fn download_for_token(&mut self, token: u32) -> Option<FileJob> {
        let item = self.downloads.iter_mut().find(|item| item.token == token)?;
        item.state = TransferState::Transferring;
        let mut job = item.file_job();
        if item.stage {
            job.root.clear();
        } else {
            job.root = self.download_dir.clone();
        }
        Some(job)
    }

    pub fn upload_for_socket(&mut self, user: &str) -> Option<FileJob> {
        let item = self
            .uploads
            .iter_mut()
            .find(|item| item.user == user && item.filing)?;
        item.filing = false;
        item.state = TransferState::Transferring;
        Some(item.file_job())
    }

    pub fn on_progress(&mut self, progress: Progress) -> Vec<Out> {
        let items = match progress.direction {
            Direction::Download => &mut self.downloads,
            Direction::Upload => &mut self.uploads,
        };
        let mut out = Vec::new();
        let mut remember = false;
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.user == progress.user && item.path == progress.path)
        {
            item.done = progress.done;
            item.speed = if progress.state == TransferState::Transferring {
                progress.speed
            } else {
                0
            };
            item.state = progress.state;
            if let Some(path) = &progress.placed {
                item.dest = path.clone();
            }
            if progress.state != TransferState::Transferring {
                item.filing = false;
                remember = true;
            }
            out.push(Out::Event(item.view(progress.direction)));
        }
        if remember {
            self.persist();
        }
        if progress.direction == Direction::Upload
            && progress.state != TransferState::Transferring
            && progress.state != TransferState::Queued
        {
            out.extend(self.pump());
        }
        if progress.direction == Direction::Download
            && progress.state == TransferState::ConnectionClosed
        {
            out.extend(self.claim_failed_albums());
        }
        out
    }

    fn enqueue_upload(&mut self, user: &str, path: &str) -> Vec<Out> {
        if self.banned.contains(user) {
            return deny(user, path, REJECT_BANNED);
        }
        if self.ignored.contains(user) {
            return deny(user, path, REJECT_CANCELLED);
        }
        let Some(shared) = self.shared.iter().find(|file| file.virtual_path == path) else {
            return deny(user, path, REJECT_FILE_NOT_SHARED);
        };
        let size = shared.size;
        let disk = shared.disk_path.clone();
        if self.upload_count() >= u64::from(self.file_limit) {
            return deny(user, path, REJECT_TOO_MANY_FILES);
        }
        if self.upload_bytes().saturating_add(size) > self.byte_limit {
            return deny(user, path, REJECT_TOO_MANY_MEGABYTES);
        }
        if let Some(item) = self
            .uploads
            .iter_mut()
            .find(|item| item.user == user && same_path(&item.path, path))
        {
            if item.state == TransferState::Transferring {
                return Vec::new();
            }
            item.started = false;
            item.filing = false;
            item.speed = 0;
            item.state = TransferState::Queued;
            item.disk = disk;
            item.size = size;
        } else {
            let seq = self.alloc_seq();
            self.uploads.push(Item {
                user: user.to_owned(),
                path: path.to_owned(),
                disk,
                dest: PathBuf::new(),
                size,
                done: 0,
                speed: 0,
                token: 0,
                state: TransferState::Queued,
                place: None,
                asked: false,
                started: false,
                filing: false,
                seq,
                reason: String::new(),
                folder: None,
                stage: false,
            });
        }
        self.persist();
        self.pump()
    }

    fn on_transfer_request(&mut self, user: &str, message: &TransferRequest) -> Vec<Out> {
        if message.direction == DIRECTION_DOWNLOAD {
            return self.enqueue_upload(user, &message.file);
        }
        let Some(item) = self
            .downloads
            .iter_mut()
            .find(|item| item.user == user && same_path(&item.path, &message.file))
        else {
            return deny_download(user, message.token);
        };
        if let Some(size) = message.filesize.filter(|size| *size > 0) {
            item.size = size;
        }
        item.token = message.token;
        item.done = std::fs::metadata(&item.disk)
            .map(|meta| meta.len())
            .unwrap_or(item.done);
        if item.done > item.size {
            item.done = item.size;
        }
        item.state = TransferState::Transferring;
        item.place = None;
        let response = TransferResponse {
            token: message.token,
            allowed: true,
            reason: None,
            filesize: Some(item.done),
        };
        let Ok(frame) = encode_transfer_response(&response) else {
            return Vec::new();
        };
        let view = item.view(Direction::Download);
        vec![
            Out::Write {
                user: user.to_owned(),
                frame,
            },
            Out::Event(view),
        ]
    }

    fn on_transfer_response(&mut self, user: &str, message: &TransferResponse) -> Vec<Out> {
        let Some(item) = self
            .uploads
            .iter_mut()
            .find(|item| item.user == user && item.token == message.token && item.started)
        else {
            return Vec::new();
        };
        if !message.allowed {
            let (state, reason) = refusal(message.reason.as_deref().unwrap_or(""));
            item.state = state;
            item.reason = reason;
            item.filing = false;
            let view = item.view(Direction::Upload);
            let mut out = vec![Out::Event(view)];
            out.extend(self.pump());
            return out;
        }
        item.filing = true;
        vec![Out::ConnectFile {
            user: user.to_owned(),
        }]
    }

    fn pump(&mut self) -> Vec<Out> {
        let mut out = Vec::new();
        while self.active_uploads() < self.slots {
            let Some(index) = self.pick_upload() else {
                break;
            };
            self.next_token = self.next_token.wrapping_add(1).max(1);
            let token = self.next_token;
            let item = &mut self.uploads[index];
            item.token = token;
            item.started = true;
            item.state = TransferState::Transferring;
            self.last_upload_user = Some(item.user.clone());
            let request = TransferRequest {
                direction: DIRECTION_UPLOAD,
                token,
                file: item.path.clone(),
                filesize: Some(item.size),
            };
            let Ok(frame) = encode_transfer_request(&request) else {
                break;
            };
            let user = item.user.clone();
            let view = item.view(Direction::Upload);
            out.push(Out::Write { user, frame });
            out.push(Out::Event(view));
        }
        for item in self
            .uploads
            .iter()
            .filter(|item| item.state == TransferState::Queued)
        {
            let place = self.place_of(&item.user, &item.path);
            if let Ok(frame) = encode_place_in_queue(&item.path, place) {
                out.push(Out::Write {
                    user: item.user.clone(),
                    frame,
                });
                out.push(Out::Event(item.view(Direction::Upload)));
            }
        }
        out
    }

    fn pick_upload(&self) -> Option<usize> {
        let waiting: Vec<usize> = self
            .uploads
            .iter()
            .enumerate()
            .filter(|(_, item)| item.state == TransferState::Queued && !item.started)
            .map(|(index, _)| index)
            .collect();
        let best = waiting
            .iter()
            .map(|index| self.rank(&self.uploads[*index].user))
            .max()?;
        let mut candidates: Vec<usize> = waiting
            .into_iter()
            .filter(|index| self.rank(&self.uploads[*index].user) == best)
            .collect();
        if self.queue_mode == QueueMode::RoundRobin
            && let Some(last) = &self.last_upload_user
            && candidates
                .iter()
                .any(|index| &self.uploads[*index].user != last)
        {
            candidates.retain(|index| &self.uploads[*index].user != last);
        }
        candidates
            .into_iter()
            .min_by_key(|index| self.uploads[*index].seq)
    }

    fn rank(&self, user: &str) -> u8 {
        u8::from(self.privileged.contains(user) || self.prioritized.contains(user))
    }

    /// Queued uploads, and whether another upload can start.
    pub fn user_info_queue(&self) -> (u32, bool) {
        let queued = self
            .uploads
            .iter()
            .filter(|item| item.state == TransferState::Queued)
            .count();
        (
            u32::try_from(queued).unwrap_or(u32::MAX),
            self.active_uploads() < self.slots,
        )
    }

    fn active_uploads(&self) -> usize {
        self.uploads
            .iter()
            .filter(|item| item.state == TransferState::Transferring)
            .count()
    }

    fn upload_count(&self) -> u64 {
        self.uploads
            .iter()
            .filter(|item| {
                matches!(
                    item.state,
                    TransferState::Queued | TransferState::Transferring
                )
            })
            .count() as u64
    }

    fn upload_bytes(&self) -> u64 {
        self.uploads
            .iter()
            .filter(|item| {
                matches!(
                    item.state,
                    TransferState::Queued | TransferState::Transferring
                )
            })
            .map(|item| item.size)
            .sum()
    }

    fn place_of(&self, user: &str, path: &str) -> u32 {
        let Some(seq) = self
            .uploads
            .iter()
            .find(|item| item.user == user && same_path(&item.path, path))
            .map(|item| item.seq)
        else {
            return 0;
        };
        let ahead = self
            .uploads
            .iter()
            .filter(|item| item.state == TransferState::Queued && item.seq < seq)
            .count();
        u32::try_from(ahead.saturating_add(1)).unwrap_or(u32::MAX)
    }

    fn note_place(&mut self, user: &str, path: &str, place: u32, out: &mut Vec<Out>) {
        if let Some(item) = self
            .downloads
            .iter_mut()
            .find(|item| item.user == user && same_path(&item.path, path))
        {
            item.place = Some(place);
            out.push(Out::Event(item.view(Direction::Download)));
        }
    }

    fn mark_download(
        &mut self,
        user: &str,
        path: &str,
        done: u64,
        state: TransferState,
        reason: &str,
        out: &mut Vec<Out>,
    ) {
        let mut changed = false;
        if let Some(item) = self
            .downloads
            .iter_mut()
            .find(|item| item.user == user && same_path(&item.path, path))
        {
            if done > 0 {
                item.done = done;
            }
            if item.state != TransferState::Finished {
                let find_another = matches!(
                    state,
                    TransferState::FileNotShared
                        | TransferState::TooManyFiles
                        | TransferState::TooManyMegabytes
                        | TransferState::InternalError
                        | TransferState::LastTryFailed
                );
                if find_another && !item.stage {
                    item.state = if state == TransferState::LastTryFailed {
                        TransferState::LastTryFailed
                    } else {
                        TransferState::Failed
                    };
                    item.speed = 0;
                    item.place = None;
                    item.done = 0;
                    item.reason.clear();
                    let disk = item.disk.clone();
                    let dest = item.dest.clone();
                    let file = file_name(&item.path).to_owned();
                    let path = item.path.clone();
                    let folder = item.folder.clone();
                    remove_partial(&disk, &dest, &self.download_dir);
                    out.push(Out::Event(item.view(Direction::Download)));
                    if !file.is_empty() {
                        out.push(Out::FileUnshared {
                            user: user.to_owned(),
                            file,
                            path,
                            folder,
                        });
                    }
                } else if find_another {
                    item.state = state;
                    item.speed = 0;
                    item.place = None;
                    item.done = 0;
                    item.reason.clear();
                    let disk = item.disk.clone();
                    let dest = item.dest.clone();
                    remove_partial(&disk, &dest, &self.download_dir);
                    out.push(Out::Event(item.view(Direction::Download)));
                } else {
                    item.state = state;
                    item.reason = if state == TransferState::Refused {
                        reason.to_owned()
                    } else {
                        String::new()
                    };
                    out.push(Out::Event(item.view(Direction::Download)));
                }
            } else {
                out.push(Out::Event(item.view(Direction::Download)));
            }
            changed = true;
        }
        if changed {
            self.persist();
        }
    }

    fn load_queue(&mut self, path: &Path) {
        let Ok(text) = fs::read_to_string(path) else {
            return;
        };
        let Ok(file) = toml::from_str::<QueueFile>(&text) else {
            return;
        };
        for saved in file.downloads {
            let Some((parsed, reason)) = state_from_label(&saved.state) else {
                continue;
            };
            let disk = incomplete_file(&self.incomplete_dir, &saved.user, &saved.path);
            let dest = if saved.stage {
                quality_stage(&self.incomplete_dir, &saved.path)
            } else {
                download_dest(&self.download_dir, saved.folder.as_deref(), &saved.path)
            };
            let state = resume_state(parsed);
            let on_disk = fs::metadata(&disk).map(|meta| meta.len()).unwrap_or(0);
            let done = if state == TransferState::Finished {
                saved.size
            } else {
                on_disk.min(saved.size)
            };
            let seq = self.alloc_seq();
            self.downloads.push(Item {
                user: saved.user,
                path: saved.path,
                disk,
                dest,
                size: saved.size,
                done,
                speed: 0,
                token: 0,
                state,
                place: None,
                asked: false,
                started: false,
                filing: false,
                seq,
                reason: if matches!(state, TransferState::Refused | TransferState::Failed) {
                    reason
                } else {
                    String::new()
                },
                folder: saved.folder.clone(),
                stage: saved.stage,
            });
        }
        for saved in file.uploads {
            let Some((parsed, _)) = state_from_label(&saved.state) else {
                continue;
            };
            let shared = self
                .shared
                .iter()
                .find(|file| file.virtual_path == saved.path);
            let (disk, size, state) = if let Some(shared) = shared {
                (shared.disk_path.clone(), shared.size, resume_state(parsed))
            } else if parsed == TransferState::Finished {
                (PathBuf::new(), saved.size, TransferState::Finished)
            } else {
                (PathBuf::new(), saved.size, TransferState::FileNotShared)
            };
            let seq = self.alloc_seq();
            self.uploads.push(Item {
                user: saved.user,
                path: saved.path,
                disk,
                dest: PathBuf::new(),
                size,
                done: if state == TransferState::Finished {
                    size
                } else {
                    0
                },
                speed: 0,
                token: 0,
                state,
                place: None,
                asked: false,
                started: state == TransferState::Finished,
                filing: false,
                seq,
                reason: String::new(),
                folder: None,
                stage: false,
            });
        }
    }

    fn persist(&self) {
        let Some(path) = &self.queue_path else {
            return;
        };
        let file = QueueFile {
            downloads: self
                .downloads
                .iter()
                .map(|item| SavedDownload {
                    user: item.user.clone(),
                    path: item.path.clone(),
                    size: item.size,
                    state: saved_label(item),
                    folder: item
                        .folder
                        .clone()
                        .or_else(|| saved_folder(&self.download_dir, &item.dest)),
                    stage: item.stage,
                })
                .collect(),
            uploads: self
                .uploads
                .iter()
                .map(|item| SavedUpload {
                    user: item.user.clone(),
                    path: item.path.clone(),
                    size: item.size,
                    state: saved_label(item),
                })
                .collect(),
        };
        let Ok(text) = toml::to_string_pretty(&file) else {
            return;
        };
        let _ = write_private(path, &text);
    }

    fn alloc_seq(&mut self) -> u64 {
        self.next_seq = self.next_seq.saturating_add(1);
        self.next_seq
    }
}

impl Drop for Book {
    fn drop(&mut self) {
        self.persist();
    }
}

#[derive(Serialize, Deserialize, Default)]
struct QueueFile {
    #[serde(default)]
    downloads: Vec<SavedDownload>,
    #[serde(default)]
    uploads: Vec<SavedUpload>,
}

#[derive(Serialize, Deserialize)]
struct SavedDownload {
    user: String,
    path: String,
    size: u64,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(default)]
    stage: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedUpload {
    user: String,
    path: String,
    size: u64,
    state: String,
}

fn resume_state(state: TransferState) -> TransferState {
    match state {
        TransferState::Finished
        | TransferState::Filtered
        | TransferState::Banned
        | TransferState::TooManyFiles
        | TransferState::TooManyMegabytes
        | TransferState::InternalError
        | TransferState::LastTryFailed
        | TransferState::PendingShutdown
        | TransferState::Refused
        | TransferState::Cancelled
        | TransferState::FileNotShared
        | TransferState::Failed => state,
        _ => TransferState::Queued,
    }
}

fn state_from_label(label: &str) -> Option<(TransferState, String)> {
    let state = match label {
        "Queued" => TransferState::Queued,
        "Transferring" => TransferState::Transferring,
        "Paused" => TransferState::Paused,
        "Finished" => TransferState::Finished,
        "Filtered" | "Blocked by filter" => TransferState::Filtered,
        "Banned" => TransferState::Banned,
        "Too many files" => TransferState::TooManyFiles,
        "Too many megabytes" => TransferState::TooManyMegabytes,
        "Internal error" => TransferState::InternalError,
        "Last try failed" => TransferState::LastTryFailed,
        "Pending shutdown" => TransferState::PendingShutdown,
        "Refused" => TransferState::Refused,
        "User logged off" => TransferState::UserLoggedOff,
        "Connection closed" => TransferState::ConnectionClosed,
        "Connection timeout" => TransferState::ConnectionTimeout,
        "File not shared" => TransferState::FileNotShared,
        "Cancelled" => TransferState::Cancelled,
        "Failed" => TransferState::Failed,
        "Failed - Unavailable" => return Some((TransferState::Failed, "Unavailable".to_owned())),
        "" => return None,
        other => return Some(refusal(other)),
    };
    Some((state, String::new()))
}

fn saved_label(item: &Item) -> String {
    if item.state == TransferState::Refused && !item.reason.is_empty() {
        item.reason.clone()
    } else if item.state == TransferState::Failed && !item.reason.is_empty() {
        format!("Failed - {}", item.reason)
    } else {
        item.state.label().to_owned()
    }
}

fn saved_folder(download_dir: &Path, dest: &Path) -> Option<String> {
    let parent = dest.parent()?;
    if parent == download_dir {
        return None;
    }
    parent.file_name()?.to_str().map(str::to_owned)
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = StdOpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn deny(user: &str, path: &str, reason: &str) -> Vec<Out> {
    let Ok(frame) = encode_upload_denied(path, reason) else {
        return Vec::new();
    };
    vec![Out::Write {
        user: user.to_owned(),
        frame,
    }]
}

fn deny_download(user: &str, token: u32) -> Vec<Out> {
    let response = TransferResponse {
        token,
        allowed: false,
        reason: Some(REJECT_CANCELLED.to_owned()),
        filesize: None,
    };
    let Ok(frame) = encode_transfer_response(&response) else {
        return Vec::new();
    };
    vec![Out::Write {
        user: user.to_owned(),
        frame,
    }]
}

fn same_path(left: &str, right: &str) -> bool {
    left == right || left.replace('/', "\\") == right.replace('/', "\\")
}

fn refusal(reason: &str) -> (TransferState, String) {
    let trimmed = reason.trim();
    // Nicotine sends "Enqueue failed due to too many files". The state column is
    // 22 characters, so that sentence stops at "due to" and hides the cause.
    let (body, shortened) = match trimmed.strip_prefix("Enqueue failed due to ") {
        Some(rest) => (rest.trim(), true),
        None => (trimmed, false),
    };
    let state = if same_reason(body, REJECT_FILE_NOT_SHARED) {
        Some(TransferState::FileNotShared)
    } else if same_reason(body, REJECT_CANCELLED) || same_reason(body, "canceled") {
        Some(TransferState::Cancelled)
    } else if same_reason(body, REJECT_QUEUED) {
        Some(TransferState::Queued)
    } else if same_reason(body, REJECT_BANNED) {
        Some(TransferState::Banned)
    } else if same_reason(body, REJECT_TOO_MANY_FILES) {
        Some(TransferState::TooManyFiles)
    } else if same_reason(body, REJECT_TOO_MANY_MEGABYTES) {
        Some(TransferState::TooManyMegabytes)
    } else if same_reason(body, "internal error") || same_reason(body, "an internal error") {
        Some(TransferState::InternalError)
    } else if same_reason(body, "recent transfer failed") {
        Some(TransferState::LastTryFailed)
    } else if same_reason(body, REJECT_PENDING_SHUTDOWN) {
        Some(TransferState::PendingShutdown)
    } else if same_reason(body, REJECT_FILTERED) {
        Some(TransferState::Filtered)
    } else {
        None
    };
    if let Some(state) = state {
        return (state, String::new());
    }
    if shortened {
        let cause = body.trim().trim_end_matches('.').trim();
        if !cause.is_empty() {
            return (TransferState::Refused, cause.to_owned());
        }
    }
    (TransferState::Refused, trimmed.to_owned())
}

fn same_reason(reason: &str, expected: &str) -> bool {
    reason_key(reason) == reason_key(expected)
}

fn reason_key(reason: &str) -> String {
    reason
        .trim()
        .trim_end_matches(['.', ' '])
        .to_ascii_lowercase()
}

fn incomplete_file(dir: &Path, user: &str, virtual_path: &str) -> PathBuf {
    let flat = virtual_path.replace(['\\', '/'], "__");
    dir.join(format!("{user}__{flat}"))
}

/// Finished path for a download. A folder download keeps the album's own name,
/// matching Nicotine+ `get_folder_destination`: parents above that folder are dropped.
/// Where a quality replacement waits until it has been measured and filed.
pub fn quality_stage(incomplete: &Path, virtual_path: &str) -> PathBuf {
    incomplete
        .join("quality")
        .join(safe_component(file_name(virtual_path)))
}

fn download_dest(root: &Path, folder: Option<&str>, virtual_path: &str) -> PathBuf {
    let mut dest = root.to_path_buf();
    if let Some(name) = folder.and_then(album_folder) {
        dest.push(name);
    }
    dest.push(safe_component(file_name(virtual_path)));
    dest
}

fn album_folder(folder: &str) -> Option<&str> {
    let name = folder
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())?;
    safe_component_opt(name)
}

fn safe_component(name: &str) -> &str {
    safe_component_opt(name).unwrap_or("file")
}

fn safe_component_opt(name: &str) -> Option<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('\0') {
        None
    } else {
        Some(name)
    }
}

fn file_name(virtual_path: &str) -> &str {
    virtual_path
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(virtual_path)
}

#[derive(Debug)]
pub enum Out {
    Write {
        user: String,
        frame: Vec<u8>,
    },
    ConnectFile {
        user: String,
    },
    Event(Transfer),
    /// Every file of this album closed. The rows are `Failed` and the partial files are gone.
    AlbumFailed {
        user: String,
        album: String,
    },
    /// This download was `File not shared`. The row is `Failed` and the partial file is gone.
    FileUnshared {
        user: String,
        file: String,
        path: String,
        folder: Option<String>,
    },
}

fn remove_partial(disk: &Path, dest: &Path, download_dir: &Path) {
    let _ = fs::remove_file(disk);
    if dest != disk {
        let _ = fs::remove_file(dest);
    }
    let mut current = dest.parent().map(Path::to_path_buf);
    while let Some(dir) = current {
        if dir == download_dir || !dir.starts_with(download_dir) {
            break;
        }
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
    }
}

#[derive(Debug)]
pub struct FileJob {
    pub user: String,
    pub path: String,
    pub disk: PathBuf,
    pub dest: PathBuf,
    /// Download folder. Empty for an upload. A finished audio file is filed under this root.
    pub root: PathBuf,
    pub size: u64,
    pub done: u64,
    pub token: u32,
}

impl Item {
    fn view(&self, direction: Direction) -> Transfer {
        Transfer {
            direction,
            user: self.user.clone(),
            path: self.path.clone(),
            size: self.size,
            done: self.done,
            speed: self.speed,
            queue: self.place,
            state: self.state,
            detail: if matches!(self.state, TransferState::Refused | TransferState::Failed) {
                self.reason.clone()
            } else {
                String::new()
            },
        }
    }

    fn file_job(&self) -> FileJob {
        FileJob {
            user: self.user.clone(),
            path: self.path.clone(),
            disk: self.disk.clone(),
            dest: self.dest.clone(),
            root: PathBuf::new(),
            size: self.size,
            done: self.done,
            token: self.token,
        }
    }
}

/// Read the uploader's token, tell them the offset, and append until `size`.
pub async fn receive_download(
    mut stream: TcpStream,
    mut leftover: Vec<u8>,
    job: FileJob,
    limit_kib: Option<u32>,
    reports: &mpsc::Sender<Progress>,
) -> Progress {
    let token = match read_mix(&mut stream, &mut leftover, 4).await {
        Ok(bytes) => u32::from_le_bytes(bytes),
        Err(()) => return closed(&job, job.done),
    };
    if token != job.token {
        return closed(&job, job.done);
    }
    if stream
        .write_all(&encode_file_offset(job.done))
        .await
        .is_err()
    {
        return closed(&job, job.done);
    }
    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&job.disk)
        .await
    {
        Ok(file) => file,
        Err(_) => return closed(&job, job.done),
    };
    let mut done = job.done;
    let mut pace = Pace::new(done);
    let mut buf = [0u8; 16 * 1024];
    while done < job.size {
        let want = usize::try_from(job.size - done)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let count = match stream.read(&mut buf[..want]).await {
            Ok(0) | Err(_) => {
                let _ = file.sync_all().await;
                return closed(&job, done);
            }
            Ok(count) => count,
        };
        if file.write_all(&buf[..count]).await.is_err() {
            return closed(&job, done);
        }
        done += u64::try_from(count).unwrap_or(0);
        pace.report(reports, Direction::Download, &job.user, &job.path, done);
        if let Some(limit) = limit_kib {
            tokio::time::sleep(chunk_delay(count, limit)).await;
        }
    }
    let _ = file.sync_all().await;
    drop(file);
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let order = MoveOrder {
        root: job.root.clone(),
        disk: job.disk.clone(),
        dest: job.dest.clone(),
        reply: reply_tx,
    };
    if move_sender().send(order).is_err() {
        return closed(&job, done);
    }
    let Ok(Ok(placed)) = reply_rx.await else {
        return closed(&job, done);
    };
    Progress {
        direction: Direction::Download,
        user: job.user,
        path: job.path,
        done,
        speed: 0,
        state: TransferState::Finished,
        placed: Some(placed.dest),
        moved: placed.moved,
    }
}

struct MoveOrder {
    root: PathBuf,
    disk: PathBuf,
    dest: PathBuf,
    reply: tokio::sync::oneshot::Sender<Result<PlacedFile, ()>>,
}

struct PlacedFile {
    dest: PathBuf,
    moved: Option<MovedFile>,
}

fn move_sender() -> &'static std::sync::mpsc::Sender<MoveOrder> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<MoveOrder>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<MoveOrder>();
        std::thread::Builder::new()
            .name("move".to_owned())
            .spawn(move || {
                while let Ok(order) = rx.recv() {
                    let placed = run_move(&order);
                    let _ = order.reply.send(placed);
                }
            })
            .expect("move thread");
        tx
    })
}

/// Shelf and rename a finished download off the runtime that reports transfer progress.
fn run_move(order: &MoveOrder) -> Result<PlacedFile, ()> {
    #[cfg(test)]
    note_move_thread();
    let dest = if order.root.as_os_str().is_empty() {
        order.dest.clone()
    } else {
        crate::library::shelf(&order.root, &order.disk).unwrap_or_else(|| order.dest.clone())
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&dest);
    if std::fs::rename(&order.disk, &dest).is_err() {
        return Err(());
    }
    let moved = (dest != order.dest).then(|| {
        let skip = order.disk.parent().and_then(|parent| {
            (parent.starts_with(&order.root) && parent != order.root).then(|| parent.to_path_buf())
        });
        MovedFile {
            root: order.root.clone(),
            from: order.dest.clone(),
            to: dest.clone(),
            swept: crate::library::sweep(&order.root, skip.as_deref()),
        }
    });
    Ok(PlacedFile { dest, moved })
}

#[cfg(test)]
fn note_move_thread() {
    *MOVE_THREAD_NAME.lock().unwrap() = std::thread::current().name().unwrap_or("").to_owned();
}

#[cfg(test)]
static MOVE_THREAD_NAME: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Write our token, read the peer's offset, and send the shared file from there.
pub async fn send_upload(
    mut stream: TcpStream,
    job: FileJob,
    limit_kib: Option<u32>,
    reports: &mpsc::Sender<Progress>,
) -> Progress {
    if stream
        .write_all(&encode_file_token(job.token))
        .await
        .is_err()
    {
        return fail_upload(&job);
    }
    let Ok(bytes) = read_exact(&mut stream, 8).await else {
        return fail_upload(&job);
    };
    let offset = u64::from_le_bytes(bytes);
    if offset > job.size {
        return fail_upload(&job);
    }
    let mut file = match tokio::fs::File::open(&job.disk).await {
        Ok(file) => file,
        Err(_) => return fail_upload(&job),
    };
    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
        return fail_upload(&job);
    }
    let mut sent = offset;
    let mut pace = Pace::new(sent);
    let mut buf = [0u8; 16 * 1024];
    while sent < job.size {
        let want = usize::try_from(job.size - sent)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let count = match file.read(&mut buf[..want]).await {
            Ok(0) | Err(_) => return fail_upload(&job),
            Ok(count) => count,
        };
        if stream.write_all(&buf[..count]).await.is_err() {
            return Progress {
                direction: Direction::Upload,
                user: job.user.clone(),
                path: job.path.clone(),
                done: sent,
                speed: 0,
                state: TransferState::ConnectionClosed,
                placed: None,
                moved: None,
            };
        }
        sent += u64::try_from(count).unwrap_or(0);
        pace.report(reports, Direction::Upload, &job.user, &job.path, sent);
        if let Some(limit) = limit_kib {
            tokio::time::sleep(chunk_delay(count, limit)).await;
        }
    }
    Progress {
        direction: Direction::Upload,
        user: job.user,
        path: job.path,
        done: sent,
        speed: 0,
        state: TransferState::Finished,
        placed: None,
        moved: None,
    }
}

fn closed(job: &FileJob, done: u64) -> Progress {
    Progress {
        direction: Direction::Download,
        user: job.user.clone(),
        path: job.path.clone(),
        done,
        speed: 0,
        state: if done >= job.size && job.size > 0 {
            TransferState::Finished
        } else {
            TransferState::ConnectionClosed
        },
        placed: None,
        moved: None,
    }
}

fn fail_upload(job: &FileJob) -> Progress {
    Progress {
        direction: Direction::Upload,
        user: job.user.clone(),
        path: job.path.clone(),
        done: job.done,
        speed: 0,
        state: TransferState::ConnectionClosed,
        placed: None,
        moved: None,
    }
}

/// Sends `Transferring` updates about every 200 ms so the progress rail and the meter move during the file.
struct Pace {
    at: std::time::Instant,
    mark: u64,
}

impl Pace {
    fn new(done: u64) -> Self {
        Self {
            at: std::time::Instant::now(),
            mark: done,
        }
    }

    fn report(
        &mut self,
        tx: &mpsc::Sender<Progress>,
        direction: Direction,
        user: &str,
        path: &str,
        done: u64,
    ) {
        let elapsed = self.at.elapsed();
        if elapsed < Duration::from_millis(200) || done == self.mark {
            return;
        }
        let micros = u64::try_from(elapsed.as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        let speed = done.saturating_sub(self.mark).saturating_mul(1_000_000) / micros;
        if tx
            .try_send(Progress {
                direction,
                user: user.to_owned(),
                path: path.to_owned(),
                done,
                speed,
                state: TransferState::Transferring,
                placed: None,
                moved: None,
            })
            .is_ok()
        {
            self.at = std::time::Instant::now();
            self.mark = done;
        }
    }
}

pub async fn read_file_token(stream: &mut TcpStream, leftover: &mut Vec<u8>) -> Result<u32, ()> {
    let bytes = read_mix(stream, leftover, 4).await?;
    Ok(u32::from_le_bytes(bytes))
}

async fn read_mix(
    stream: &mut TcpStream,
    leftover: &mut Vec<u8>,
    len: usize,
) -> Result<[u8; 4], ()> {
    let mut buf = [0u8; 4];
    let have = leftover.len().min(len);
    buf[..have].copy_from_slice(&leftover[..have]);
    leftover.drain(..have);
    if have < len {
        stream
            .read_exact(&mut buf[have..len])
            .await
            .map_err(|_| ())?;
    }
    Ok(buf)
}

async fn read_exact(stream: &mut TcpStream, len: usize) -> Result<[u8; 8], ()> {
    let mut buf = [0u8; 8];
    stream.read_exact(&mut buf[..len]).await.map_err(|_| ())?;
    Ok(buf)
}

/// Tell the downloader the upload socket closed early.
pub fn upload_failed_frame(path: &str) -> Option<Vec<u8>> {
    encode_upload_failed(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Buddy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn book(slots: u16, files: u32, megabytes: u32) -> Book {
        let config = Config {
            upload_slots: slots,
            queue_file_limit: files,
            queue_megabytes: megabytes,
            banned: vec!["eve".to_owned()],
            buddies: vec![Buddy {
                name: "vip".to_owned(),
                note: String::new(),
                notify: false,
                prioritized: true,
                trusted: false,
            }],
            ..Config::default()
        };
        let shared = vec![Shared {
            virtual_path: "public\\tone.bin".to_owned(),
            disk_path: PathBuf::from("tone.bin"),
            size: 64,
        }];
        Book::new(&config, shared)
    }

    fn queue_book(dir: &std::path::Path) -> Book {
        let config = Config {
            incomplete_dir: dir.join("incomplete").to_string_lossy().into_owned(),
            download_dir: dir.join("done").to_string_lossy().into_owned(),
            ..Config::default()
        };
        let shared = vec![Shared {
            virtual_path: "public\\tone.bin".to_owned(),
            disk_path: PathBuf::from("tone.bin"),
            size: 64,
        }];
        Book::new(&config, shared)
    }

    fn download_book(dir: &std::path::Path) -> Book {
        let config = Config {
            incomplete_dir: dir.join("incomplete").to_string_lossy().into_owned(),
            download_dir: dir.join("done").to_string_lossy().into_owned(),
            ..Config::default()
        };
        Book::new(&config, Vec::new())
    }

    fn codes(out: &[Out]) -> Vec<u32> {
        out.iter()
            .filter_map(|item| match item {
                Out::Write { frame, .. } => {
                    Some(u32::from_le_bytes(frame[4..8].try_into().unwrap()))
                }
                Out::ConnectFile { .. }
                | Out::Event(_)
                | Out::AlbumFailed { .. }
                | Out::FileUnshared { .. } => None,
            })
            .collect()
    }

    #[test]
    fn third_upload_waits_for_a_free_slot() {
        let mut book = book(2, 100, 10_000);
        let first = book.on_peer(
            "bob",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        assert!(codes(&first).contains(&TRANSFER_REQUEST));
        let second = book.on_peer(
            "carol",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        assert!(codes(&second).contains(&TRANSFER_REQUEST));
        let third = book.on_peer(
            "dave",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        assert!(!codes(&third).contains(&TRANSFER_REQUEST));
        assert!(codes(&third).contains(&PLACE_IN_QUEUE_RESPONSE));
        assert_eq!(book.active_uploads(), 2);
    }

    #[test]
    fn banned_user_is_denied() {
        let mut book = book(2, 100, 10_000);
        let out = book.on_peer(
            "eve",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        let frame = match &out[0] {
            Out::Write { frame, .. } => frame.clone(),
            _ => panic!("expected a write"),
        };
        let denied = decode_upload_denied(&frame[8..]).unwrap();
        assert_eq!(denied.reason, REJECT_BANNED);
    }

    #[test]
    fn ignored_user_is_cancelled() {
        let mut book = book(2, 100, 10_000);
        book.set_blocks(&[], &["iris".to_owned()], &[]);
        let out = book.on_peer(
            "iris",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        let frame = match &out[0] {
            Out::Write { frame, .. } => frame.clone(),
            _ => panic!("expected a write"),
        };
        assert_eq!(
            decode_upload_denied(&frame[8..]).unwrap().reason,
            REJECT_CANCELLED
        );
    }

    #[test]
    fn missing_file_and_caps_use_the_nicotine_reasons() {
        let mut book = book(2, 1, 10_000);
        let missing = book.on_peer(
            "bob",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\nope.bin").unwrap()[8..],
        );
        let frame = match &missing[0] {
            Out::Write { frame, .. } => frame.clone(),
            _ => panic!("expected a write"),
        };
        assert_eq!(
            decode_upload_denied(&frame[8..]).unwrap().reason,
            REJECT_FILE_NOT_SHARED
        );
        let _ = book.on_peer(
            "bob",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        let capped = book.on_peer(
            "carol",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        let frame = match &capped[0] {
            Out::Write { frame, .. } => frame.clone(),
            _ => panic!("expected a write"),
        };
        assert_eq!(
            decode_upload_denied(&frame[8..]).unwrap().reason,
            REJECT_TOO_MANY_FILES
        );
    }

    #[test]
    fn prioritized_buddy_starts_before_an_earlier_user() {
        let mut book = book(1, 100, 10_000);
        let _ = book.on_peer(
            "bob",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        book.uploads[0].started = false;
        book.uploads[0].state = TransferState::Queued;
        let vip = book.on_peer(
            "vip",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        let user = vip.iter().find_map(|item| match item {
            Out::Write { user, frame, .. }
                if u32::from_le_bytes(frame[4..8].try_into().unwrap()) == TRANSFER_REQUEST =>
            {
                Some(user.clone())
            }
            _ => None,
        });
        assert_eq!(user.as_deref(), Some("vip"));
    }

    #[test]
    fn apply_prefs_updates_slots_and_the_upload_limit() {
        let mut book = Book::new(&Config::default(), Vec::new());
        let mut prefs = crate::settings::LivePrefs::from_config(&Config::default());
        prefs.upload_slots = 4;
        prefs.upload_limit_kib = Some(32);
        book.apply_prefs(&prefs);
        assert_eq!(book.slots, 4);
        assert_eq!(book.upload_limit_kib, Some(32));
    }

    #[test]
    fn an_album_download_keeps_the_folder_name() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-album-dest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Dookie\\01.flac".to_owned(),
            size: 4,
            folder: Some("music\\Dookie".to_owned()),
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Nimrod\\01.flac".to_owned(),
            size: 4,
            folder: Some("music\\Nimrod".to_owned()),
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\tone.bin".to_owned(),
            size: 4,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\tone.bin".to_owned(),
            size: 4,
            folder: Some("..".to_owned()),
            stage: false,
        })
        .unwrap();
        let done = dir.join("done");
        assert_eq!(book.downloads[0].dest, done.join("Dookie").join("01.flac"));
        assert!(done.join("Dookie").is_dir());
        assert_eq!(book.downloads[1].dest, done.join("Nimrod").join("01.flac"));
        assert_ne!(book.downloads[0].disk, book.downloads[1].disk);
        assert_eq!(book.downloads.len(), 3);
        assert_eq!(book.downloads[2].dest, done.join("tone.bin"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_queued_download_shows_its_place_and_a_failed_peer_says_why() {
        let dir = std::env::temp_dir().join(format!("soul-sever-queue-why-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        assert!(book.place_requests("bob").is_empty());
        assert_eq!(book.take_queue_uploads("bob").len(), 1);
        let asks = book.place_requests("bob");
        assert_eq!(
            u32::from_le_bytes(asks[0][4..8].try_into().unwrap()),
            PLACE_IN_QUEUE_REQUEST
        );
        let frame = encode_place_in_queue("a.flac", 4).unwrap();
        let out = book.on_peer("bob", PLACE_IN_QUEUE_RESPONSE, &frame[8..]);
        let Out::Event(transfer) = &out[0] else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.queue, Some(4));
        assert_eq!(transfer.state, TransferState::Queued);
        let failed = book.fail_queued("bob", TransferState::ConnectionTimeout);
        let Out::Event(transfer) = &failed[0] else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.state, TransferState::ConnectionTimeout);
        assert_eq!(transfer.state.label(), "Connection timeout");
        assert_eq!(transfer.queue, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_closed_album_is_failed_and_its_files_are_removed() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-album-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        let queue = dir.join("queue.toml");
        book.use_queue(queue.clone());
        for (path, folder) in [
            ("music\\Dookie\\01.flac", Some("music\\Dookie")),
            ("music\\Dookie\\02.flac", Some("music\\Dookie")),
            ("tone.bin", None),
        ] {
            book.enqueue_download(&DownloadRequest {
                user: "bob".to_owned(),
                path: path.to_owned(),
                size: 10,
                folder: folder.map(str::to_owned),
                stage: false,
            })
            .unwrap();
        }
        let disk = book.downloads[0].disk.clone();
        let dest = book.downloads[0].dest.clone();
        std::fs::write(&disk, b"partial").unwrap();
        std::fs::write(&dest, b"partial").unwrap();
        let out = book.fail_queued("bob", TransferState::ConnectionClosed);
        assert!(out.iter().any(|item| matches!(
            item,
            Out::AlbumFailed { user, album } if user == "bob" && album == "Dookie"
        )));
        assert_eq!(book.downloads[0].state, TransferState::Failed);
        assert_eq!(book.downloads[0].state.label(), "Failed");
        assert_eq!(book.downloads[1].state, TransferState::Failed);
        assert_eq!(book.downloads[2].state, TransferState::ConnectionClosed);
        assert_eq!(book.downloads[0].done, 0);
        assert!(!disk.exists());
        assert!(!dest.exists());
        assert!(!dest.parent().unwrap().exists());
        let (_, users) = book.retry_connection_failures();
        assert_eq!(users, vec!["bob".to_owned()]);
        assert_eq!(book.downloads[0].state, TransferState::Failed);
        assert_eq!(book.downloads[1].state, TransferState::Failed);
        assert_eq!(book.downloads[2].state, TransferState::Queued);
        let mut again = download_book(&dir);
        again.use_queue(queue.clone());
        assert_eq!(again.downloads[0].state, TransferState::Failed);
        assert_eq!(again.downloads[1].state, TransferState::Failed);
        assert_eq!(again.downloads[2].state, TransferState::Queued);
        let marked = again.mark_unavailable("bob", "Dookie");
        assert!(marked.iter().any(|item| matches!(
            item,
            Out::Event(transfer) if transfer.status() == "Failed - Unavailable"
        )));
        assert_eq!(
            again.downloads[0].view(Direction::Download).status(),
            "Failed - Unavailable"
        );
        assert_eq!(
            again.downloads[1].view(Direction::Download).status(),
            "Failed - Unavailable"
        );
        assert_eq!(again.downloads[2].state, TransferState::Queued);
        let mut kept = download_book(&dir);
        kept.use_queue(queue);
        assert_eq!(
            kept.downloads[0].view(Direction::Download).status(),
            "Failed - Unavailable"
        );
        assert_eq!(
            kept.downloads[1].view(Direction::Download).status(),
            "Failed - Unavailable"
        );
        assert_eq!(kept.downloads[2].state, TransferState::Queued);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_not_shared_is_failed_and_asks_for_a_new_copy() {
        let dir = std::env::temp_dir().join(format!("soul-sever-unshared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Dookie\\01.flac".to_owned(),
            size: 10,
            folder: Some("music\\Dookie".to_owned()),
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Dookie\\02.flac".to_owned(),
            size: 10,
            folder: Some("music\\Dookie".to_owned()),
            stage: false,
        })
        .unwrap();
        let disk = book.downloads[0].disk.clone();
        let dest = book.downloads[0].dest.clone();
        std::fs::write(&disk, b"partial").unwrap();
        std::fs::write(&dest, b"partial").unwrap();
        let frame = encode_upload_denied("music\\Dookie\\01.flac", REJECT_FILE_NOT_SHARED).unwrap();
        let out = book.on_peer("bob", UPLOAD_DENIED, &frame[8..]);
        assert!(out.iter().any(|item| matches!(
            item,
            Out::FileUnshared {
                user,
                file,
                path,
                folder,
            } if user == "bob"
                && file == "01.flac"
                && path == "music\\Dookie\\01.flac"
                && folder.as_deref() == Some("music\\Dookie")
        )));
        assert_eq!(book.downloads[0].state, TransferState::Failed);
        assert_eq!(book.downloads[1].state, TransferState::Queued);
        assert!(!disk.exists());
        assert!(!dest.exists());
        let marked = book.mark_file_unavailable("bob", "music\\Dookie\\01.flac");
        assert!(marked.iter().any(|item| matches!(
            item,
            Out::Event(transfer) if transfer.status() == "Failed - Unavailable"
        )));
        assert_eq!(book.downloads[1].state, TransferState::Queued);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_full_queue_asks_for_another_copy() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-full-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        for reason in [
            REJECT_TOO_MANY_FILES,
            REJECT_TOO_MANY_MEGABYTES,
            "Internal error",
            "Enqueue failed due to an internal error",
            "Enqueue failed due to recent transfer failed",
            "Recent transfer failed",
        ] {
            let path = format!("music\\Dookie\\{reason}.flac");
            book.enqueue_download(&DownloadRequest {
                user: "bob".to_owned(),
                path: path.clone(),
                size: 10,
                folder: Some("music\\Dookie".to_owned()),
                stage: false,
            })
            .unwrap();
            let frame = encode_upload_denied(&path, reason).unwrap();
            let out = book.on_peer("bob", UPLOAD_DENIED, &frame[8..]);
            assert!(
                out.iter().any(|item| matches!(
                    item,
                    Out::FileUnshared { path: denied, .. } if denied == &path
                )),
                "{reason}"
            );
            let row = book
                .downloads
                .iter()
                .find(|item| item.path == path)
                .unwrap();
            let kept = matches!(
                reason,
                "Enqueue failed due to recent transfer failed" | "Recent transfer failed"
            );
            assert_eq!(
                row.state,
                if kept {
                    TransferState::LastTryFailed
                } else {
                    TransferState::Failed
                },
                "{reason}"
            );
            if kept {
                assert_eq!(row.view(Direction::Download).status(), "Last try failed");
            }
        }
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Dookie\\staged.flac".to_owned(),
            size: 10,
            folder: None,
            stage: true,
        })
        .unwrap();
        let frame = encode_upload_denied("music\\Dookie\\staged.flac", "Internal error").unwrap();
        let staged = book.on_peer("bob", UPLOAD_DENIED, &frame[8..]);
        assert!(
            staged
                .iter()
                .all(|item| !matches!(item, Out::FileUnshared { .. }))
        );
        assert_eq!(
            book.downloads
                .iter()
                .find(|item| item.path == "music\\Dookie\\staged.flac")
                .unwrap()
                .state,
            TransferState::InternalError
        );
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\Dookie\\recent.flac".to_owned(),
            size: 10,
            folder: None,
            stage: true,
        })
        .unwrap();
        let frame = encode_upload_denied(
            "music\\Dookie\\recent.flac",
            "Enqueue failed due to recent transfer failed",
        )
        .unwrap();
        let recent = book.on_peer("bob", UPLOAD_DENIED, &frame[8..]);
        assert!(
            recent
                .iter()
                .all(|item| !matches!(item, Out::FileUnshared { .. }))
        );
        assert_eq!(
            book.downloads
                .iter()
                .find(|item| item.path == "music\\Dookie\\recent.flac")
                .unwrap()
                .state,
            TransferState::LastTryFailed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn one_open_file_keeps_the_album_on_the_same_peer() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-album-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        for path in ["music\\Dookie\\01.flac", "music\\Dookie\\02.flac"] {
            book.enqueue_download(&DownloadRequest {
                user: "bob".to_owned(),
                path: path.to_owned(),
                size: 10,
                folder: Some("music\\Dookie".to_owned()),
                stage: false,
            })
            .unwrap();
        }
        book.downloads[1].state = TransferState::Transferring;
        let out = book.fail_queued("bob", TransferState::ConnectionClosed);
        assert!(
            out.iter()
                .all(|item| !matches!(item, Out::AlbumFailed { .. }))
        );
        assert_eq!(book.downloads[0].state, TransferState::ConnectionClosed);
        assert_eq!(book.downloads[1].state, TransferState::Transferring);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refused_download_shows_the_peers_reason() {
        let dir = std::env::temp_dir().join(format!("soul-sever-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        let queue = dir.join("queue.toml");
        book.use_queue(queue.clone());
        let cases = [
            ("a.flac", REJECT_BANNED, TransferState::Banned, "Banned"),
            (
                "b.flac",
                REJECT_TOO_MANY_FILES,
                TransferState::Failed,
                "Failed",
            ),
            (
                "c.flac",
                REJECT_TOO_MANY_MEGABYTES,
                TransferState::Failed,
                "Failed",
            ),
            (
                "d.flac",
                REJECT_PENDING_SHUTDOWN,
                TransferState::PendingShutdown,
                "Pending shutdown",
            ),
            (
                "e.flac",
                REJECT_FILTERED,
                TransferState::Filtered,
                "Blocked by filter",
            ),
            (
                "f.flac",
                "Sharing off",
                TransferState::Refused,
                "Sharing off",
            ),
            (
                "g.flac",
                "Enqueue failed due to too many files",
                TransferState::Failed,
                "Failed",
            ),
            (
                "h.flac",
                "Enqueue failed due to too many megabytes.",
                TransferState::Failed,
                "Failed",
            ),
            (
                "i.flac",
                "Enqueue failed due to a daily limit",
                TransferState::Refused,
                "a daily limit",
            ),
        ];
        for (path, reason, state, label) in cases {
            book.enqueue_download(&DownloadRequest {
                user: "bob".to_owned(),
                path: path.to_owned(),
                size: 4,
                folder: None,
                stage: false,
            })
            .unwrap();
            let frame = encode_upload_denied(path, reason).unwrap();
            let out = book.on_peer("bob", UPLOAD_DENIED, &frame[8..]);
            let transfer = out
                .iter()
                .find_map(|item| match item {
                    Out::Event(transfer) if transfer.path == path => Some(transfer),
                    _ => None,
                })
                .expect(path);
            assert_eq!(transfer.state, state);
            assert_eq!(transfer.status(), label);
        }
        let mut again = download_book(&dir);
        again.use_queue(queue);
        let sharing = again
            .downloads
            .iter()
            .find(|item| item.path == "f.flac")
            .unwrap();
        assert_eq!(sharing.view(Direction::Download).status(), "Sharing off");
        let filtered = again
            .downloads
            .iter()
            .find(|item| item.path == "e.flac")
            .unwrap();
        assert_eq!(filtered.state, TransferState::Filtered);
        assert_eq!(
            filtered.view(Direction::Download).status(),
            "Blocked by filter"
        );
        assert_eq!(
            again
                .downloads
                .iter()
                .find(|item| item.path == "g.flac")
                .unwrap()
                .view(Direction::Download)
                .status(),
            "Failed"
        );
        assert_eq!(
            again
                .downloads
                .iter()
                .find(|item| item.path == "i.flac")
                .unwrap()
                .view(Direction::Download)
                .status(),
            "a daily limit"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn connection_failures_are_asked_again_and_keep_the_bytes() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-retry-conn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "b.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "carol".to_owned(),
            path: "c.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.downloads[0].done = 4;
        book.downloads[0].asked = true;
        book.downloads[0].state = TransferState::ConnectionClosed;
        book.downloads[1].state = TransferState::ConnectionTimeout;
        book.downloads[2].state = TransferState::UserLoggedOff;
        let (out, users) = book.retry_connection_failures();
        assert_eq!(users, vec!["bob".to_owned()]);
        assert_eq!(out.len(), 2);
        let Out::Event(transfer) = &out[0] else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.state, TransferState::Queued);
        assert_eq!(transfer.done, 4);
        assert!(!book.downloads[0].asked);
        assert_eq!(book.downloads[2].state, TransferState::UserLoggedOff);
        assert_eq!(book.take_queue_uploads("bob").len(), 2);
        assert!(book.take_queue_uploads("carol").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_offer_with_forward_slashes_leaves_the_queue() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-slash-offer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\tone.bin".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        let request = TransferRequest {
            direction: DIRECTION_UPLOAD,
            token: 7,
            file: "music/tone.bin".to_owned(),
            filesize: Some(0),
        };
        let frame = encode_transfer_request(&request).unwrap();
        let out = book.on_peer("bob", TRANSFER_REQUEST, &frame[8..]);
        let Some(Out::Event(transfer)) = out.iter().find(|item| matches!(item, Out::Event(_)))
        else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.state, TransferState::Transferring);
        assert_eq!(book.downloads[0].size, 10);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_saved_queue_returns_on_the_next_open() {
        let dir =
            std::env::temp_dir().join(format!("soul-sever-queue-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let incomplete = dir.join("incomplete");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::write(incomplete.join("bob__music__other.bin"), [1, 2, 3]).unwrap();
        let path = dir.join("queue.toml");
        let mut book = queue_book(&dir);
        book.use_queue(path.clone());
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\tone.bin".to_owned(),
            size: 10,
            folder: Some("music\\Dookie".to_owned()),
            stage: false,
        })
        .unwrap();
        book.downloads[0].state = TransferState::Finished;
        book.downloads[0].done = 10;
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "music\\other.bin".to_owned(),
            size: 8,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.downloads[1].state = TransferState::ConnectionClosed;
        book.enqueue_download(&DownloadRequest {
            user: "cara".to_owned(),
            path: "note.txt".to_owned(),
            size: 4,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.downloads[2].state = TransferState::Filtered;
        let _ = book.on_peer(
            "ned",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        book.uploads[0].state = TransferState::Finished;
        book.persist();
        drop(book);

        let mut again = queue_book(&dir);
        again.use_queue(path);
        assert_eq!(again.downloads[0].state, TransferState::Finished);
        assert_eq!(
            again.downloads[0].dest,
            dir.join("done").join("Dookie").join("tone.bin")
        );
        assert_eq!(again.downloads[1].state, TransferState::Queued);
        assert_eq!(again.downloads[1].done, 3);
        assert_eq!(again.downloads[2].state, TransferState::Filtered);
        assert_eq!(again.queued_download_users(), vec!["bob".to_owned()]);
        assert_eq!(again.uploads[0].state, TransferState::Finished);
        again.clear_finished(true, false);
        assert!(
            again
                .downloads
                .iter()
                .all(|item| item.state != TransferState::Finished)
        );
        assert_eq!(again.uploads[0].state, TransferState::Finished);
        let text = std::fs::read_to_string(dir.join("queue.toml")).unwrap();
        assert!(!text.contains("Dookie"), "{text}");
        assert!(text.contains("other.bin"), "{text}");
        assert!(text.contains("public\\tone.bin"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_during_a_download_keeps_the_bytes_and_the_speed() {
        let dir = std::env::temp_dir().join(format!("soul-sever-pace-row-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            size: 10_000,
            folder: None,
            stage: false,
        })
        .unwrap();
        let out = book.on_progress(Progress {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            done: 2_500,
            speed: 8_000,
            state: TransferState::Transferring,
            placed: None,
            moved: None,
        });
        let Some(Out::Event(transfer)) = out.iter().find(|item| matches!(item, Out::Event(_)))
        else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.done, 2_500);
        assert_eq!(transfer.speed, 8_000);
        assert_eq!(transfer.percent(), 25);
        let finished = book.on_progress(Progress {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            done: 10_000,
            speed: 8_000,
            state: TransferState::Finished,
            placed: None,
            moved: None,
        });
        let Some(Out::Event(transfer)) = finished.iter().find(|item| matches!(item, Out::Event(_)))
        else {
            panic!("expected a transfer");
        };
        assert_eq!(transfer.state, TransferState::Finished);
        assert_eq!(transfer.done, 10_000);
        assert_eq!(transfer.speed, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_download_reports_progress_before_the_file_finishes() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-pace-bytes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let job = FileJob {
            user: "bob".to_owned(),
            path: "music\\tone.bin".to_owned(),
            disk: dir.join("incomplete"),
            dest: dir.join("tone.bin"),
            root: PathBuf::new(),
            size: 48 * 1024,
            done: 0,
            token: 7,
        };
        let client = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let task =
            tokio::spawn(async move { receive_download(client, Vec::new(), job, None, &tx).await });
        let (mut peer, _) = listener.accept().await.unwrap();
        peer.write_all(&7u32.to_le_bytes()).await.unwrap();
        let mut offset = [0u8; 8];
        peer.read_exact(&mut offset).await.unwrap();
        assert_eq!(u64::from_le_bytes(offset), 0);
        let chunk = vec![9u8; 16 * 1024];
        peer.write_all(&chunk).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        peer.write_all(&chunk).await.unwrap();
        let mid = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(mid.state, TransferState::Transferring);
        assert!(mid.done > 0 && mid.done < 48 * 1024, "{}", mid.done);
        assert!(mid.speed > 0, "{}", mid.speed);
        peer.write_all(&chunk).await.unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, TransferState::Finished);
        assert_eq!(finished.done, 48 * 1024);
        assert_eq!(finished.speed, 0);
        assert_eq!(
            std::fs::read(dir.join("tone.bin")).unwrap().len(),
            48 * 1024
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chunk_delay_matches_a_kibibyte_limit() {
        assert_eq!(chunk_delay(1024, 1), Duration::from_secs(1));
        assert_eq!(chunk_delay(512, 1), Duration::from_millis(500));
    }

    #[test]
    fn cancelling_a_queued_download_drops_it_and_refuses_a_later_offer() {
        let dir = std::env::temp_dir().join(format!("soul-sever-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut book = download_book(&dir);
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        book.enqueue_download(&DownloadRequest {
            user: "bob".to_owned(),
            path: "b.flac".to_owned(),
            size: 10,
            folder: None,
            stage: false,
        })
        .unwrap();
        let dropped = book.cancel(Direction::Download, "bob", "a.flac");
        assert!(dropped.is_empty());
        assert_eq!(book.downloads.len(), 1);
        assert_eq!(book.downloads[0].path, "b.flac");
        let request = TransferRequest {
            direction: DIRECTION_UPLOAD,
            token: 7,
            file: "a.flac".to_owned(),
            filesize: Some(10),
        };
        let out = book.on_peer(
            "bob",
            TRANSFER_REQUEST,
            &encode_transfer_request(&request).unwrap()[8..],
        );
        let frame = match &out[0] {
            Out::Write { frame, .. } => frame.clone(),
            _ => panic!("expected a write"),
        };
        let response = decode_transfer_response(&frame[8..]).unwrap();
        assert!(!response.allowed);
        assert_eq!(response.reason.as_deref(), Some(REJECT_CANCELLED));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancelling_a_queued_upload_denies_it() {
        let mut book = book(1, 100, 10_000);
        let started = book.on_peer(
            "bob",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        assert!(codes(&started).contains(&TRANSFER_REQUEST));
        let waiting = book.on_peer(
            "carol",
            QUEUE_UPLOAD,
            &encode_queue_upload("public\\tone.bin").unwrap()[8..],
        );
        assert!(!codes(&waiting).contains(&TRANSFER_REQUEST));
        let out = book.cancel(Direction::Upload, "carol", "public\\tone.bin");
        let frame = match out.iter().find(|item| matches!(item, Out::Write { .. })) {
            Some(Out::Write { frame, .. }) => frame.clone(),
            _ => panic!("expected UploadDenied"),
        };
        assert_eq!(
            decode_upload_denied(&frame[8..]).unwrap().reason,
            REJECT_CANCELLED
        );
        assert_eq!(book.uploads.len(), 1);
        assert_eq!(book.uploads[0].user, "bob");
        let finished = book.cancel(Direction::Upload, "bob", "public\\tone.bin");
        assert!(!codes(&finished).contains(&UPLOAD_DENIED));
        assert!(book.uploads.is_empty());
    }

    #[tokio::test]
    async fn a_finished_download_is_filed_by_its_tags() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-shelf-xfer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("loose.wav");
        write_tagged_wav(&staged);
        let download = dir.join("done");
        let leftover = download.join("Dookie").join("folder.jpg");
        let nested = download.join("Dookie").join("scans").join("front.png");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&leftover, b"cover").unwrap();
        std::fs::write(&nested, b"scan").unwrap();
        let bytes = std::fs::read(&staged).unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let job = FileJob {
            user: "bob".to_owned(),
            path: "music\\Dookie\\01.flac".to_owned(),
            disk: dir.join("incomplete.wav"),
            dest: dir.join("done").join("Dookie").join("01.flac"),
            root: dir.join("done"),
            size: bytes.len() as u64,
            done: 0,
            token: 7,
        };
        let client = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let task =
            tokio::spawn(async move { receive_download(client, Vec::new(), job, None, &tx).await });
        let (mut peer, _) = listener.accept().await.unwrap();
        peer.write_all(&7u32.to_le_bytes()).await.unwrap();
        let mut offset = [0u8; 8];
        peer.read_exact(&mut offset).await.unwrap();
        peer.write_all(&bytes).await.unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished.state, TransferState::Finished);
        assert_eq!(MOVE_THREAD_NAME.lock().unwrap().as_str(), "move");
        let filed = dir
            .join("done")
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        assert_eq!(finished.placed.as_deref(), Some(filed.as_path()));
        let moved = finished.moved.expect("tagged download moved");
        assert_eq!(moved.to, filed);
        assert_eq!(moved.from, dir.join("done").join("Dookie").join("01.flac"));
        assert_eq!(moved.root, dir.join("done"));
        assert!(filed.is_file(), "{}", filed.display());
        assert!(!dir.join("done").join("Dookie").exists());
        assert!(!leftover.exists());
        assert!(!nested.exists());
        assert!(moved.swept.contains(&leftover));
        assert!(moved.swept.contains(&nested));
        assert!(dir.join("done").is_dir());
        let catalog = crate::library::Catalog::scan(&dir.join("done"));
        assert_eq!(catalog.artists[0].name, "Green Day");
        assert_eq!(catalog.artists[0].albums[0].name, "Dookie");
        assert_eq!(catalog.artists[0].albums[0].songs[0].title, "Basket Case");
        let mut shares = crate::config::Shares {
            public: vec![dir.join("done")],
            ..crate::config::Shares::default()
        };
        let mut index = crate::shares::ShareIndex::empty();
        index.store_moved(
            &mut shares,
            &dir.join("done"),
            &dir.join("done").join("Dookie").join("01.flac"),
            &filed,
        );
        let shared = shared_files(&index);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].disk_path, filed);
        assert_eq!(
            shared[0].virtual_path,
            format!(
                "{}\\Green Day\\Dookie\\01 Basket Case.wav",
                dir.join("done").file_name().unwrap().to_str().unwrap()
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn write_tagged_wav(path: &Path) {
        let rate = 8_000u32;
        let mut samples = Vec::new();
        for index in 0..rate {
            let step = index as f32 / rate as f32;
            let value = (step * 440.0 * std::f32::consts::TAU).sin();
            samples.push((value * 12_000.0) as i16);
        }
        let mut bytes = Vec::new();
        let data_len = u32::try_from(samples.len() * 2).unwrap();
        bytes.extend(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{Accessor, Tag, TagType};
        let mut file = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist("Green Day".to_owned());
        tag.set_album("Dookie".to_owned());
        tag.set_title("Basket Case".to_owned());
        tag.set_track(1);
        file.insert_tag(tag);
        file.save_to_path(path, WriteOptions::default()).unwrap();
    }
}
