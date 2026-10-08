//! Peer initialization, then the server-style frames that follow it.
//!
//! Nicotine+ 3.3.11 reads a peer-init message only when at least 5 bytes are buffered:
//! `uint32 size` (includes the type byte, excludes itself) and `uint8 type`.
//! `0` is [`PierceFireWall`](PeerHandshake::PierceFireWall). `1` is [`PeerHandshake::PeerInit`].
//! A size below 1 is shorter than that 5-byte minimum and is rejected before the body is read.
//! The cap is 16 KiB.
//!
//! After the handshake, peer messages use the same 8-byte header as server frames
//! (`uint32 size` including a `uint32` code) and the numbers in [`super::codes::peer`].

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use super::codes::peer_init;
use super::wire::{MAX_MESSAGE_16K, ProtocolError, Reader, Writer, encode_frame};

/// Uncompressed share-list cap. The peer frame around it stays at 16 MiB.
const MAX_SHARE_LIST: u64 = 64 * 1024 * 1024;

/// Nicotine+ `FileAttribute` numbers that this client writes and reads.
pub const FILE_ATTRIBUTE_BITRATE: u32 = 0;
pub const FILE_ATTRIBUTE_DURATION: u32 = 1;
pub const FILE_ATTRIBUTE_VBR: u32 = 2;
pub const FILE_ATTRIBUTE_SAMPLE_RATE: u32 = 4;
pub const FILE_ATTRIBUTE_BIT_DEPTH: u32 = 5;

pub use super::codes::peer::*;

/// Soulseek peer connection type. The wire value is a one-byte string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnType {
    Peer,
    File,
    Distributed,
}

impl ConnType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Peer => "P",
            Self::File => "F",
            Self::Distributed => "D",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ProtocolError> {
        match value {
            "P" => Ok(Self::Peer),
            "F" => Ok(Self::File),
            "D" => Ok(Self::Distributed),
            _ => Err(ProtocolError::UnknownConnectionType {
                value: value.to_owned(),
            }),
        }
    }
}

/// The first message on a new peer socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerHandshake {
    PierceFireWall {
        token: u32,
    },
    PeerInit {
        username: String,
        conn_type: ConnType,
    },
}

/// Incremental reader for peer-init frames. Not the 8-byte peer-message header.
#[derive(Debug, Default)]
pub struct PeerInitDecoder {
    buf: Vec<u8>,
}

impl PeerInitDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    pub fn into_remainder(self) -> Vec<u8> {
        self.buf
    }

    pub fn pop(&mut self) -> Result<Option<PeerHandshake>, ProtocolError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let size = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
        if size < 1 {
            return Err(ProtocolError::PeerInitTooSmall { size });
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
        let type_byte = self.buf[4];
        let content = self.buf[5..total].to_vec();
        self.buf.drain(..total);
        Ok(Some(decode_handshake(type_byte, &content)?))
    }
}

pub fn encode_pierce_firewall(token: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(token);
    encode_init(peer_init::PIERCE_FIRE_WALL, &writer.finish())
}

pub fn encode_peer_init(username: &str, conn_type: ConnType) -> Result<Vec<u8>, ProtocolError> {
    check_username(username)?;
    let mut writer = Writer::new();
    writer.string(username)?;
    writer.string(conn_type.as_str())?;
    writer.u32(0);
    encode_init(peer_init::PEER_INIT, &writer.finish())
}

fn encode_init(code: u32, content: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let type_byte = u8::try_from(code).map_err(|_| ProtocolError::IntegerOverflow)?;
    let size = u32::try_from(1 + content.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
    let mut out = Vec::with_capacity(4 + 1 + content.len());
    out.extend_from_slice(&size.to_le_bytes());
    out.push(type_byte);
    out.extend_from_slice(content);
    Ok(out)
}

fn decode_handshake(type_byte: u8, content: &[u8]) -> Result<PeerHandshake, ProtocolError> {
    let mut reader = super::wire::Reader::new(content);
    match u32::from(type_byte) {
        peer_init::PIERCE_FIRE_WALL => Ok(PeerHandshake::PierceFireWall {
            token: reader.u32()?,
        }),
        peer_init::PEER_INIT => {
            let username = reader.string()?;
            check_username(&username)?;
            let conn_type = ConnType::parse(&reader.string()?)?;
            Ok(PeerHandshake::PeerInit {
                username,
                conn_type,
            })
        }
        _ => Err(ProtocolError::UnknownPeerInit { code: type_byte }),
    }
}

/// One file inside a shared directory on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFile {
    pub name: String,
    pub size: u64,
    pub attributes: Vec<(u32, u32)>,
}

/// One directory in a share list. `directory` uses `\` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFolder {
    pub directory: String,
    pub files: Vec<SharedFile>,
}

/// Peer code 5. The payload is zlib of the directory list, then `u32` 0,
/// then a second directory list of folders the requester is not allowed to browse.
///
/// Nicotine+ 3.3.11 `SharedFileListResponse`: each directory is a string and a
/// file count. Each file is `u8` 1, a string name, a `u64` size, an obsolete
/// extension length (0), an attribute count, and `u32` code / `u32` value pairs.
/// Codes are bitrate 0, duration 1, VBR 2, sample rate 4, and bit depth 5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFileListResponse {
    pub list: Vec<SharedFolder>,
    pub private_list: Vec<SharedFolder>,
}

/// Peer code 4. Nicotine+ writes an empty payload.
pub fn encode_shared_file_list_request() -> Result<Vec<u8>, ProtocolError> {
    encode_frame(SHARED_FILE_LIST_REQUEST, &[])
}

impl SharedFileListResponse {
    /// Zlib-compressed payload. Wrap it with [`encode_frame`] and
    /// [`SHARED_FILE_LIST_RESPONSE`] for the peer socket.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let raw = pack_share_list(self)?;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&raw)
            .map_err(|_| ProtocolError::ShareListCorrupt)?;
        encoder
            .finish()
            .map_err(|_| ProtocolError::ShareListCorrupt)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let decoder = ZlibDecoder::new(payload);
        let mut raw = Vec::new();
        decoder
            .take(MAX_SHARE_LIST + 1)
            .read_to_end(&mut raw)
            .map_err(|_| ProtocolError::ShareListCorrupt)?;
        if raw.len() as u64 > MAX_SHARE_LIST {
            return Err(ProtocolError::ShareListTooLarge {
                max: MAX_SHARE_LIST,
            });
        }
        unpack_share_list(&raw)
    }
}

fn pack_share_list(response: &SharedFileListResponse) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    write_folders(&mut writer, &sorted_folders(&response.list))?;
    writer.u32(0);
    if !response.private_list.is_empty() {
        write_folders(&mut writer, &sorted_folders(&response.private_list))?;
    }
    Ok(writer.finish())
}

fn sorted_folders(folders: &[SharedFolder]) -> Vec<SharedFolder> {
    let mut folders = folders.to_vec();
    folders.sort_by(|left, right| left.directory.cmp(&right.directory));
    for folder in &mut folders {
        folder
            .files
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
    folders
}

fn write_file(
    writer: &mut Writer,
    name: &str,
    size: u64,
    attributes: &[(u32, u32)],
) -> Result<(), ProtocolError> {
    writer.u8(1);
    writer.string(name)?;
    writer.u64(size);
    writer.u32(0);
    let attribute_count =
        u32::try_from(attributes.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
    writer.u32(attribute_count);
    for (code, value) in attributes {
        writer.u32(*code);
        writer.u32(*value);
    }
    Ok(())
}

fn write_folders(writer: &mut Writer, folders: &[SharedFolder]) -> Result<(), ProtocolError> {
    let count = u32::try_from(folders.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
    writer.u32(count);
    for folder in folders {
        writer.string(&folder.directory.replace('/', "\\"))?;
        let file_count =
            u32::try_from(folder.files.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
        writer.u32(file_count);
        for file in &folder.files {
            write_file(writer, &file.name, file.size, &file.attributes)?;
        }
    }
    Ok(())
}

fn unpack_share_list(raw: &[u8]) -> Result<SharedFileListResponse, ProtocolError> {
    let mut reader = Reader::new(raw);
    let list = read_folders(&mut reader)?;
    let private_list = if reader.rest().is_empty() {
        Vec::new()
    } else {
        let _unknown = reader.u32()?;
        if reader.rest().is_empty() {
            Vec::new()
        } else {
            read_folders(&mut reader)?
        }
    };
    Ok(SharedFileListResponse { list, private_list })
}

fn read_folders(reader: &mut Reader<'_>) -> Result<Vec<SharedFolder>, ProtocolError> {
    let count = reader.u32()?;
    let mut folders = Vec::new();
    for _ in 0..count {
        let directory = reader.string()?.replace('/', "\\");
        let file_count = reader.u32()?;
        let mut files = Vec::new();
        for _ in 0..file_count {
            let _code = reader.u8()?;
            let name = reader.string()?;
            let size = read_file_size(reader)?;
            let extension_len =
                usize::try_from(reader.u32()?).map_err(|_| ProtocolError::IntegerOverflow)?;
            reader.skip(extension_len)?;
            let attribute_count = reader.u32()?;
            let mut attributes = Vec::new();
            for _ in 0..attribute_count {
                let code = reader.u32()?;
                let value = reader.u32()?;
                if is_file_attribute(code) {
                    attributes.push((code, value));
                }
            }
            files.push(SharedFile {
                name,
                size,
                attributes,
            });
        }
        files.sort_by(|left, right| left.name.cmp(&right.name));
        folders.push(SharedFolder { directory, files });
    }
    folders.sort_by(|left, right| left.directory.cmp(&right.directory));
    Ok(folders)
}

fn is_file_attribute(code: u32) -> bool {
    matches!(
        code,
        FILE_ATTRIBUTE_BITRATE
            | FILE_ATTRIBUTE_DURATION
            | FILE_ATTRIBUTE_VBR
            | FILE_ATTRIBUTE_SAMPLE_RATE
            | FILE_ATTRIBUTE_BIT_DEPTH
    )
}

/// Soulseek NS wrote sizes above 2 GiB as a `u32` followed by `0xffffffff`.
fn read_file_size(reader: &mut Reader<'_>) -> Result<u64, ProtocolError> {
    let rest = reader.rest();
    if rest.len() >= 8 && rest[7] == 0xff {
        let size = u64::from(reader.u32()?);
        let _high = reader.u32()?;
        Ok(size)
    } else {
        reader.u64()
    }
}

/// One file in a peer code 9 search response. `path` uses `\` separators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultFile {
    pub path: String,
    pub size: u64,
    pub attributes: Vec<(u32, u32)>,
}

/// Peer code 9. The payload is zlib of the match list Nicotine+ writes in
/// `FileSearchResponse.make_network_message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearchResponse {
    pub username: String,
    pub token: u32,
    pub files: Vec<SearchResultFile>,
    pub free_slot: bool,
    pub upload_speed: u32,
    pub queue: u32,
    pub private_files: Vec<SearchResultFile>,
}

impl FileSearchResponse {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut writer = Writer::new();
        writer.string(&self.username)?;
        writer.u32(self.token);
        write_result_files(&mut writer, &self.files)?;
        writer.u8(u8::from(self.free_slot));
        writer.u32(self.upload_speed);
        writer.u32(self.queue);
        writer.u32(0);
        if !self.private_files.is_empty() {
            write_result_files(&mut writer, &self.private_files)?;
        }
        let raw = writer.finish();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&raw)
            .map_err(|_| ProtocolError::ShareListCorrupt)?;
        encoder
            .finish()
            .map_err(|_| ProtocolError::ShareListCorrupt)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let decoder = ZlibDecoder::new(payload);
        let mut raw = Vec::new();
        decoder
            .take(MAX_SHARE_LIST + 1)
            .read_to_end(&mut raw)
            .map_err(|_| ProtocolError::ShareListCorrupt)?;
        if raw.len() as u64 > MAX_SHARE_LIST {
            return Err(ProtocolError::ShareListTooLarge {
                max: MAX_SHARE_LIST,
            });
        }
        let mut reader = Reader::new(&raw);
        let username = reader.string()?;
        let token = reader.u32()?;
        let files = read_result_files(&mut reader)?;
        let free_slot = reader.bool()?;
        let upload_speed = reader.u32()?;
        let queue = reader.u32()?;
        if !reader.rest().is_empty() {
            let _unknown = reader.u32()?;
        }
        let private_files = if reader.rest().is_empty() {
            Vec::new()
        } else {
            read_result_files(&mut reader)?
        };
        Ok(Self {
            username,
            token,
            files,
            free_slot,
            upload_speed,
            queue,
            private_files,
        })
    }
}

fn write_result_files(
    writer: &mut Writer,
    files: &[SearchResultFile],
) -> Result<(), ProtocolError> {
    let count = u32::try_from(files.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
    writer.u32(count);
    for file in files {
        write_file(
            writer,
            &file.path.replace('/', "\\"),
            file.size,
            &file.attributes,
        )?;
    }
    Ok(())
}

fn read_result_files(reader: &mut Reader<'_>) -> Result<Vec<SearchResultFile>, ProtocolError> {
    let count = reader.u32()?;
    let mut files = Vec::new();
    for _ in 0..count {
        let _code = reader.u8()?;
        let path = reader.string()?.replace('/', "\\");
        let size = read_file_size(reader)?;
        let extension_len =
            usize::try_from(reader.u32()?).map_err(|_| ProtocolError::IntegerOverflow)?;
        reader.skip(extension_len)?;
        let attribute_count = reader.u32()?;
        let mut attributes = Vec::new();
        for _ in 0..attribute_count {
            let code = reader.u32()?;
            let value = reader.u32()?;
            if is_file_attribute(code) {
                attributes.push((code, value));
            }
        }
        files.push(SearchResultFile {
            path,
            size,
            attributes,
        });
    }
    Ok(files)
}

/// Download request on a peer socket. The uploader later sends [`TransferRequest`].
pub const DIRECTION_DOWNLOAD: u32 = 0;
/// Uploader is ready to send the file.
pub const DIRECTION_UPLOAD: u32 = 1;

pub const REJECT_QUEUED: &str = "Queued";
pub const REJECT_FILE_NOT_SHARED: &str = "File not shared.";
pub const REJECT_BANNED: &str = "Banned";
pub const REJECT_TOO_MANY_FILES: &str = "Too many files";
pub const REJECT_TOO_MANY_MEGABYTES: &str = "Too many megabytes";
pub const REJECT_PENDING_SHUTDOWN: &str = "Pending shutdown.";
pub const REJECT_FILTERED: &str = "Filtered";
pub const REJECT_CANCELLED: &str = "Cancelled";

/// Peer code 40. `filesize` is present only for an upload offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRequest {
    pub direction: u32,
    pub token: u32,
    pub file: String,
    pub filesize: Option<u64>,
}

/// Peer code 41. An allowed download carries the bytes already on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferResponse {
    pub token: u32,
    pub allowed: bool,
    pub reason: Option<String>,
    pub filesize: Option<u64>,
}

/// Peer code 43. Payload is the virtual path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueUpload {
    pub file: String,
}

/// Peer code 44.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceInQueueResponse {
    pub file: String,
    pub place: u32,
}

/// Peer code 46.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadFailed {
    pub file: String,
}

/// Peer code 50.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadDenied {
    pub file: String,
    pub reason: String,
}

/// Peer code 51.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceInQueueRequest {
    pub file: String,
}

fn peer_frame(code: u32, payload: Vec<u8>) -> Result<Vec<u8>, ProtocolError> {
    encode_frame(code, &payload)
}

pub fn encode_queue_upload(file: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(file)?;
    peer_frame(QUEUE_UPLOAD, writer.finish())
}

pub fn decode_queue_upload(payload: &[u8]) -> Result<QueueUpload, ProtocolError> {
    Ok(QueueUpload {
        file: Reader::new(payload).string()?,
    })
}

pub fn encode_transfer_request(request: &TransferRequest) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(request.direction);
    writer.u32(request.token);
    writer.string(&request.file)?;
    if request.direction == DIRECTION_UPLOAD {
        writer.u64(request.filesize.unwrap_or(0));
    }
    peer_frame(TRANSFER_REQUEST, writer.finish())
}

pub fn decode_transfer_request(payload: &[u8]) -> Result<TransferRequest, ProtocolError> {
    let mut reader = Reader::new(payload);
    let direction = reader.u32()?;
    let token = reader.u32()?;
    let file = reader.string()?;
    let filesize = if direction == DIRECTION_UPLOAD {
        Some(reader.u64()?)
    } else {
        None
    };
    Ok(TransferRequest {
        direction,
        token,
        file,
        filesize,
    })
}

pub fn encode_transfer_response(response: &TransferResponse) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(response.token);
    writer.u8(u8::from(response.allowed));
    if response.allowed {
        if let Some(filesize) = response.filesize {
            writer.u64(filesize);
        }
    } else if let Some(reason) = &response.reason {
        writer.string(reason)?;
    }
    peer_frame(TRANSFER_RESPONSE, writer.finish())
}

pub fn decode_transfer_response(payload: &[u8]) -> Result<TransferResponse, ProtocolError> {
    let mut reader = Reader::new(payload);
    let token = reader.u32()?;
    let allowed = reader.bool()?;
    let (reason, filesize) = if reader.rest().is_empty() {
        (None, None)
    } else if allowed {
        (None, Some(reader.u64()?))
    } else {
        (Some(reader.string()?), None)
    };
    Ok(TransferResponse {
        token,
        allowed,
        reason,
        filesize,
    })
}

pub fn encode_upload_denied(file: &str, reason: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(file)?;
    writer.string(reason)?;
    peer_frame(UPLOAD_DENIED, writer.finish())
}

pub fn decode_upload_denied(payload: &[u8]) -> Result<UploadDenied, ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(UploadDenied {
        file: reader.string()?,
        reason: reader.string()?,
    })
}

pub fn encode_upload_failed(file: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(file)?;
    peer_frame(UPLOAD_FAILED, writer.finish())
}

pub fn decode_upload_failed(payload: &[u8]) -> Result<UploadFailed, ProtocolError> {
    Ok(UploadFailed {
        file: Reader::new(payload).string()?,
    })
}

pub fn encode_place_in_queue(file: &str, place: u32) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(file)?;
    writer.u32(place);
    peer_frame(PLACE_IN_QUEUE_RESPONSE, writer.finish())
}

pub fn decode_place_in_queue(payload: &[u8]) -> Result<PlaceInQueueResponse, ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(PlaceInQueueResponse {
        file: reader.string()?,
        place: reader.u32()?,
    })
}

pub fn encode_place_request(file: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(file)?;
    peer_frame(PLACE_IN_QUEUE_REQUEST, writer.finish())
}

pub fn decode_place_request(payload: &[u8]) -> Result<PlaceInQueueRequest, ProtocolError> {
    Ok(PlaceInQueueRequest {
        file: Reader::new(payload).string()?,
    })
}

/// First four bytes on an `F` connection after `PeerInit`.
pub fn encode_file_token(token: u32) -> [u8; 4] {
    token.to_le_bytes()
}

/// Eight bytes the downloader writes after the token.
pub fn encode_file_offset(offset: u64) -> [u8; 8] {
    offset.to_le_bytes()
}

/// Peer code 16. Description, optional picture, upload total, queue size, free slot, upload permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfoResponse {
    pub description: String,
    pub picture: Option<Vec<u8>>,
    pub total_uploads: u32,
    pub queue_size: u32,
    pub slots_available: bool,
    pub upload_allowed: u32,
}

/// Peer code 15. Nicotine+ writes an empty payload.
pub fn encode_user_info_request() -> Result<Vec<u8>, ProtocolError> {
    encode_frame(USER_INFO_REQUEST, &[])
}

pub fn encode_user_info_response(info: &UserInfoResponse) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.string(&info.description)?;
    match &info.picture {
        Some(picture) => {
            writer.u8(1);
            let len = u32::try_from(picture.len()).map_err(|_| ProtocolError::IntegerOverflow)?;
            writer.u32(len);
            writer.raw(picture);
        }
        None => writer.u8(0),
    }
    writer.u32(info.total_uploads);
    writer.u32(info.queue_size);
    writer.u8(u8::from(info.slots_available));
    writer.u32(info.upload_allowed);
    peer_frame(USER_INFO_RESPONSE, writer.finish())
}

pub fn decode_user_info_response(payload: &[u8]) -> Result<UserInfoResponse, ProtocolError> {
    let mut reader = Reader::new(payload);
    let description = reader.string()?;
    let picture = if reader.bool()? {
        let len = usize::try_from(reader.u32()?).map_err(|_| ProtocolError::IntegerOverflow)?;
        Some(reader.bytes(len)?.to_vec())
    } else {
        None
    };
    let total_uploads = reader.u32()?;
    let queue_size = reader.u32()?;
    let slots_available = reader.bool()?;
    let upload_allowed = if reader.rest().len() >= 4 {
        reader.u32()?
    } else {
        0
    };
    Ok(UserInfoResponse {
        description,
        picture,
        total_uploads,
        queue_size,
        slots_available,
        upload_allowed,
    })
}

/// Peer code 36. Token, then the directory name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderContentsRequest {
    pub token: u32,
    pub directory: String,
}

pub fn encode_folder_contents_request(
    request: &FolderContentsRequest,
) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(request.token);
    writer.string(&request.directory)?;
    peer_frame(FOLDER_CONTENTS_REQUEST, writer.finish())
}

pub fn decode_folder_contents_request(
    payload: &[u8],
) -> Result<FolderContentsRequest, ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(FolderContentsRequest {
        token: reader.u32()?,
        directory: reader.string()?,
    })
}

/// Peer code 37. The payload is zlib of a token, a directory, then the same folder records as a share list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderContentsResponse {
    pub token: u32,
    pub directory: String,
    pub folders: Vec<SharedFolder>,
}

pub fn encode_folder_contents_response(
    response: &FolderContentsResponse,
) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::new();
    writer.u32(response.token);
    writer.string(&response.directory)?;
    write_folders(&mut writer, &response.folders)?;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&writer.finish())
        .map_err(|_| ProtocolError::ShareListCorrupt)?;
    let compressed = encoder
        .finish()
        .map_err(|_| ProtocolError::ShareListCorrupt)?;
    peer_frame(FOLDER_CONTENTS_RESPONSE, compressed)
}

pub fn decode_folder_contents_response(
    payload: &[u8],
) -> Result<FolderContentsResponse, ProtocolError> {
    let decoder = ZlibDecoder::new(payload);
    let mut raw = Vec::new();
    decoder
        .take(MAX_SHARE_LIST + 1)
        .read_to_end(&mut raw)
        .map_err(|_| ProtocolError::ShareListCorrupt)?;
    if raw.len() as u64 > MAX_SHARE_LIST {
        return Err(ProtocolError::ShareListTooLarge {
            max: MAX_SHARE_LIST,
        });
    }
    let mut reader = Reader::new(&raw);
    Ok(FolderContentsResponse {
        token: reader.u32()?,
        directory: reader.string()?,
        folders: read_folders(&mut reader)?,
    })
}

fn check_username(username: &str) -> Result<(), ProtocolError> {
    let bytes = username.len();
    if bytes == 0 || bytes > 256 || username == "server" || username.chars().any(char::is_control) {
        return Err(ProtocolError::RejectedUsername);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use flate2::Compression;
    use flate2::read::ZlibDecoder;
    use flate2::write::ZlibEncoder;

    use super::*;
    use crate::protocol::{FrameDecoder, encode_frame};

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn peer_init_matches_the_nicotine_layout() {
        let frame = encode_peer_init("alice", ConnType::Peer).unwrap();
        assert_eq!(frame, hex("130000000105000000616c696365010000005000000000"));
        let mut decoder = PeerInitDecoder::new();
        decoder.push(&frame[..4]);
        assert!(decoder.pop().unwrap().is_none());
        decoder.push(&frame[4..]);
        assert_eq!(
            decoder.pop().unwrap().unwrap(),
            PeerHandshake::PeerInit {
                username: "alice".to_owned(),
                conn_type: ConnType::Peer,
            }
        );
    }

    #[test]
    fn pierce_firewall_carries_the_token() {
        let frame = encode_pierce_firewall(7).unwrap();
        assert_eq!(frame, hex("050000000007000000"));
        let mut decoder = PeerInitDecoder::new();
        decoder.push(&frame);
        assert_eq!(
            decoder.pop().unwrap().unwrap(),
            PeerHandshake::PierceFireWall { token: 7 }
        );
    }

    #[test]
    fn peer_init_shorter_than_five_bytes_fails() {
        let mut decoder = PeerInitDecoder::new();
        decoder.push(&hex("00000000"));
        assert_eq!(
            decoder.pop().unwrap_err(),
            ProtocolError::PeerInitTooSmall { size: 0 }
        );
    }

    #[test]
    fn oversized_peer_init_fails_before_the_body() {
        let mut decoder = PeerInitDecoder::new();
        decoder.push(&16385u32.to_le_bytes());
        assert_eq!(
            decoder.pop().unwrap_err(),
            ProtocolError::MessageTooLarge {
                size: 16_385,
                max: MAX_MESSAGE_16K,
            }
        );
    }

    #[test]
    fn peer_messages_use_the_server_frame() {
        let frame = encode_frame(SHARED_FILE_LIST_REQUEST, &[]).unwrap();
        assert_eq!(frame, hex("0400000004000000"));
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        let decoded = decoder.pop().unwrap().unwrap();
        assert_eq!(decoded.code, SHARED_FILE_LIST_REQUEST);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn shared_file_list_request_is_an_empty_frame() {
        assert_eq!(
            encode_shared_file_list_request().unwrap(),
            hex("0400000004000000")
        );
    }

    #[test]
    fn shared_file_list_response_matches_the_nicotine_layout() {
        let response = SharedFileListResponse {
            list: vec![SharedFolder {
                directory: "music".to_owned(),
                files: vec![SharedFile {
                    name: "song.mp3".to_owned(),
                    size: 9,
                    attributes: vec![
                        (FILE_ATTRIBUTE_BITRATE, 320),
                        (FILE_ATTRIBUTE_DURATION, 180),
                        (FILE_ATTRIBUTE_VBR, 1),
                    ],
                }],
            }],
            private_list: Vec::new(),
        };
        let payload = response.encode().unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(payload.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(
            raw,
            hex(
                "01000000050000006d75736963010000000108000000736f6e672e6d703309000000000000000000000003000000000000004001000001000000b4000000020000000100000000000000"
            )
        );
        assert_eq!(SharedFileListResponse::decode(&payload).unwrap(), response);
    }

    #[test]
    fn private_share_list_follows_the_unknown_zero() {
        let response = SharedFileListResponse {
            list: vec![folder("public", "a.txt")],
            private_list: vec![folder("buddy", "b.txt")],
        };
        let decoded = SharedFileListResponse::decode(&response.encode().unwrap()).unwrap();
        assert_eq!(decoded.list[0].files[0].name, "a.txt");
        assert_eq!(decoded.private_list[0].files[0].name, "b.txt");
    }

    #[test]
    fn soulseek_ns_size_keeps_the_low_word() {
        let raw =
            hex("0100000001000000640100000001010000006105000000ffffffff000000000000000000000000");
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        let decoded = SharedFileListResponse::decode(&encoder.finish().unwrap()).unwrap();
        assert_eq!(decoded.list[0].files[0].size, 5);
    }

    #[test]
    fn file_search_response_matches_the_nicotine_layout() {
        let response = FileSearchResponse {
            username: "alice".to_owned(),
            token: 7,
            files: vec![SearchResultFile {
                path: "music\\a.mp3".to_owned(),
                size: 9,
                attributes: vec![
                    (FILE_ATTRIBUTE_BITRATE, 320),
                    (FILE_ATTRIBUTE_DURATION, 180),
                ],
            }],
            free_slot: true,
            upload_speed: 10,
            queue: 2,
            private_files: Vec::new(),
        };
        let payload = response.encode().unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(payload.as_slice())
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(
            raw,
            hex(
                "05000000616c6963650700000001000000010b0000006d757369635c612e6d703309000000000000000000000002000000000000004001000001000000b4000000010a0000000200000000000000"
            )
        );
        assert_eq!(FileSearchResponse::decode(&payload).unwrap(), response);
    }

    #[test]
    fn transfer_messages_match_the_nicotine_layout() {
        let file = "music\\a.bin";
        assert_eq!(
            encode_queue_upload(file).unwrap(),
            hex("130000002b0000000b0000006d757369635c612e62696e")
        );
        let request = TransferRequest {
            direction: DIRECTION_UPLOAD,
            token: 7,
            file: file.to_owned(),
            filesize: Some(65536),
        };
        let raw = encode_transfer_request(&request).unwrap();
        assert_eq!(
            raw,
            hex("230000002800000001000000070000000b0000006d757369635c612e62696e0000010000000000")
        );
        assert_eq!(decode_transfer_request(&raw[8..]).unwrap(), request);
        let response = TransferResponse {
            token: 7,
            allowed: true,
            reason: None,
            filesize: Some(1024),
        };
        let raw = encode_transfer_response(&response).unwrap();
        assert_eq!(raw, hex("110000002900000007000000010004000000000000"));
        assert_eq!(decode_transfer_response(&raw[8..]).unwrap(), response);
        let raw = encode_upload_denied(file, REJECT_BANNED).unwrap();
        assert_eq!(
            raw,
            hex("1d000000320000000b0000006d757369635c612e62696e0600000042616e6e6564")
        );
        assert_eq!(
            decode_upload_denied(&raw[8..]).unwrap().reason,
            REJECT_BANNED
        );
        assert_eq!(
            encode_place_in_queue(file, 1).unwrap(),
            hex("170000002c0000000b0000006d757369635c612e62696e01000000")
        );
        assert_eq!(encode_file_token(7), [7, 0, 0, 0]);
        assert_eq!(encode_file_offset(1024), [0, 4, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn user_info_and_folder_contents_match_the_nicotine_layout() {
        assert_eq!(encode_user_info_request().unwrap(), hex("040000000f000000"));
        let info = UserInfoResponse {
            description: "hi".to_owned(),
            picture: None,
            total_uploads: 3,
            queue_size: 1,
            slots_available: true,
            upload_allowed: 2,
        };
        let frame = encode_user_info_response(&info).unwrap();
        assert_eq!(
            frame,
            hex("18000000100000000200000068690003000000010000000102000000")
        );
        assert_eq!(decode_user_info_response(&frame[8..]).unwrap(), info);
        let request = FolderContentsRequest {
            token: 7,
            directory: "music".to_owned(),
        };
        assert_eq!(
            encode_folder_contents_request(&request).unwrap(),
            hex("110000002400000007000000050000006d75736963")
        );
        let compressed = hex(
            "789ce364606060656060c82d2dce4c6644e630313030308278897a4999792c0ca8002c930492e14093010092430802",
        );
        let folder = decode_folder_contents_response(&compressed).unwrap();
        assert_eq!(folder.token, 9);
        assert_eq!(folder.folders[0].files.len(), 2);
        assert_eq!(folder.folders[0].files[0].name, "a.bin");
        assert_eq!(folder.folders[0].files[1].size, 8);
    }

    fn folder(directory: &str, name: &str) -> SharedFolder {
        SharedFolder {
            directory: directory.to_owned(),
            files: vec![SharedFile {
                name: name.to_owned(),
                size: 1,
                attributes: Vec::new(),
            }],
        }
    }
}
