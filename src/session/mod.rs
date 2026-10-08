//! Server login. The task owns the TCP stream and emits [`SessionEvent`] values.
//!
//! Tests talk to [`fixture`] on `127.0.0.1`. This module does not dial a host
//! unless [`Session::spawn`] is called with that host.

mod browse;
mod chat;
#[cfg(test)]
mod fixture;
mod peers;
mod portmap;
mod search;
mod transfers;

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::config::Config;
use crate::model::{Direction, Transfer, TransferState};
use crate::protocol::{
    ClientMessage, ConnType, FileSearchResponse, FrameDecoder, IncomingConnect, LoginRequest,
    LoginResponse, PeerAddress, PeerHandshake, ProtocolError, ServerFrame, SetStatus, UserStatus,
    decode_cant_connect, decode_incoming_connect, decode_incoming_file_search,
    decode_login_response, decode_peer_address, decode_possible_parents, decode_recommendations,
    decode_similar_users, decode_user_interests, decode_user_stats, decode_user_status,
    decode_watch_user, decode_wishlist_interval, server,
};

pub use search::SearchRequest;
pub use transfers::DownloadRequest;
pub(crate) use transfers::quality_stage;

use peers::{MAX_PEERS, PeerBook, SessionCommand};

/// How long [`Session::spawn`] waits for the TCP connect, the login write, and the reply.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Nicotine+ asks again for downloads left at connection closed or connection timeout.
pub const RETRY_CONNECTION: Duration = Duration::from_secs(180);
/// Idle gap after the last write before a server ping. Nicotine+ documents one minute.
pub const PING_INTERVAL: Duration = Duration::from_secs(60);
/// Wait after a failed server reconnect before dialing again.
const SERVER_RECONNECT: Duration = Duration::from_secs(5);

/// One outcome from the server login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    LoggedIn {
        banner: String,
        ip: Ipv4Addr,
        supporter: bool,
    },
    LoginFailed {
        reason: String,
    },
    Listening {
        port: u16,
        /// Interface address in use. A specific bind is that address. An empty bind is the local address of the server connection.
        address: String,
    },
    ListenFailed {
        message: String,
    },
    UserStatus {
        user: String,
        status: u32,
        privileged: bool,
    },
    Kicked,
    PeerReady {
        username: String,
        conn_type: ConnType,
        path: PeerPath,
    },
    PeerFailed {
        username: String,
        message: String,
    },
    /// The interactive search now uses this token. Earlier interactive tokens are closed.
    SearchBegan(u32),
    /// A peer `FileSearchResponse` for `token`.
    SearchResult(u32, crate::model::SearchHit),
    /// One hit from a library upgrade search. It does not belong on the search screen.
    QualityHit(crate::model::SearchHit),
    Folder {
        user: String,
        directory: String,
        files: Vec<crate::model::SearchHit>,
    },
    Network {
        parent: Option<String>,
        children: u32,
    },
    ShareScanFailed {
        message: String,
    },
    Transfer(Transfer),
    /// Every file of an album download closed. Those rows are `Failed` and their files are gone.
    AlbumFailed {
        user: String,
        album: String,
    },
    /// One download was `File not shared`. That row is `Failed` and its partial file is gone.
    FileUnshared {
        user: String,
        file: String,
        path: String,
        folder: Option<String>,
    },
    Browse {
        user: String,
        rows: Vec<crate::model::BrowseRow>,
    },
    UserInfo {
        user: String,
        description: String,
        total_uploads: u32,
        queue_size: u32,
        slots_available: bool,
    },
    Watched {
        user: String,
        status: u32,
        country: String,
    },
    Interests {
        user: String,
        likes: Vec<String>,
        hates: Vec<String>,
    },
    Recommendations(Vec<crate::protocol::RatedItem>),
    SimilarUsers(Vec<crate::protocol::RatedItem>),
    UserStats {
        user: String,
        files: u32,
        dirs: u32,
    },
    Chat(crate::model::ChatLine),
    RoomMembers {
        room: String,
        members: Vec<crate::model::RoomPerson>,
    },
    RoomListed {
        room: String,
        users: u32,
    },
    Ticker {
        room: String,
        text: String,
    },
    /// NAT-PMP or UPnP published `external`. The server connection stays up.
    PortMapped {
        external: u16,
    },
    /// Port mapping failed. The server connection stays up.
    PortMapFailed {
        message: String,
    },
    /// File and folder counts from the local share index.
    Shares {
        public_files: u32,
        public_folders: u32,
        buddy_files: u32,
        buddy_folders: u32,
        trusted_files: u32,
        trusted_folders: u32,
    },
    /// `path` was not a share root. It is now a public share so a filed download stays shared.
    ShareRoot {
        path: PathBuf,
    },
    TimedOut,
    Disconnected {
        message: String,
    },
    /// One line for the bottom log. The text matches a Nicotine+ 3.3.11 log line.
    Log(String),
}

/// How a peer socket was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerPath {
    /// We connected and sent `PeerInit`.
    DirectOut,
    /// They connected and sent `PeerInit`.
    DirectIn,
    /// Our direct connect was refused. They sent `PierceFireWall` with our token.
    Indirect { token: u32 },
    /// The server asked us to pierce their firewall.
    Pierce { token: u32 },
}

/// Handle for the login task. Dropping it closes the server socket.
pub struct Session {
    shutdown: watch::Sender<bool>,
    commands: mpsc::Sender<SessionCommand>,
    task: Option<JoinHandle<()>>,
}

impl Session {
    /// Starts a login against `config.server_host` using [`CONNECT_TIMEOUT`].
    pub fn spawn(config: Config, events: mpsc::Sender<SessionEvent>) -> Self {
        Self::spawn_timeout(config, events, CONNECT_TIMEOUT)
    }

    pub fn spawn_timeout(
        config: Config,
        events: mpsc::Sender<SessionEvent>,
        timeout: Duration,
    ) -> Self {
        Self::start(config, events, timeout, PING_INTERVAL)
    }

    /// Sends the server message for `request`. Wishlist mode waits for `WishlistInterval`.
    pub async fn search(&self, request: SearchRequest) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Search {
                mode: request.mode,
                query: request.query,
                room: request.room,
                user: request.user,
            })
            .await;
    }

    /// Applies slots, speed limits, folders, search limits, and chat options.
    pub async fn apply_prefs(&self, prefs: crate::settings::LivePrefs) {
        let _ = self
            .commands
            .send(peers::SessionCommand::ApplyPrefs { prefs })
            .await;
    }

    /// Drops deleted library files from the share index and tells the server.
    pub async fn forget(&self, paths: Vec<std::path::PathBuf>) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Forget { paths })
            .await;
    }

    /// Points the share index at files a library scan moved.
    pub async fn relocate(&self, moves: Vec<crate::library::Relocation>) {
        if moves.is_empty() {
            return;
        }
        let _ = self
            .commands
            .send(peers::SessionCommand::Relocate { moves })
            .await;
    }

    /// Replaces the share index and emits new file and folder counts.
    pub async fn rescan(&self, shares: crate::config::Shares) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Rescan { shares })
            .await;
    }

    /// Asks `user` for the files in `directory`.
    pub async fn folder(&self, user: String, directory: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Folder { user, directory })
            .await;
    }

    /// Asks `user` for their share list and user info.
    pub async fn browse(&self, user: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Browse { user })
            .await;
    }

    /// Sends `SayChatroom` after censor and word-replace. Resolves once the frame is written.
    pub async fn say(&self, room: String, text: String) {
        let (ack, done) = tokio::sync::oneshot::channel();
        let _ = self
            .commands
            .send(peers::SessionCommand::Say { room, text, ack })
            .await;
        let _ = done.await;
    }

    /// Sends `JoinRoom` for `room`.
    pub async fn join(&self, room: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Join { room })
            .await;
    }

    /// Asks the server for `user`'s status, file counts, and interests.
    pub async fn inspect(&self, user: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Inspect { user })
            .await;
    }

    /// Sends `SetStatus` away or online and remembers that for auto-reply.
    pub async fn set_away(&self, away: bool) {
        let (ack, done) = tokio::sync::oneshot::channel();
        let _ = self
            .commands
            .send(peers::SessionCommand::SetAway { away, ack })
            .await;
        let _ = done.await;
    }

    /// Replaces the ban, ignore, and priority sets. Resolves after the task applies them.
    pub async fn set_lists(
        &self,
        banned: Vec<String>,
        ignored: Vec<String>,
        prioritized: Vec<String>,
    ) {
        let (ack, done) = tokio::sync::oneshot::channel();
        let _ = self
            .commands
            .send(peers::SessionCommand::SetLists {
                banned,
                ignored,
                prioritized,
                ack,
            })
            .await;
        let _ = done.await;
    }

    /// Marks one failed file `Failed - Unavailable` after no free slot was found.
    pub async fn file_unavailable(&self, user: String, path: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::FileUnavailable { user, path })
            .await;
    }

    /// Marks a failed album `Failed - Unavailable` after no free slot was found.
    pub async fn album_unavailable(&self, user: String, album: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::AlbumUnavailable { user, album })
            .await;
    }

    /// Queues `request` and sends `QueueUpload` once that peer is connected.
    pub async fn download(&self, request: DownloadRequest) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Download {
                user: request.user,
                path: request.path,
                size: request.size,
                folder: request.folder,
                stage: request.stage,
            })
            .await;
    }

    /// Asks the network for a better copy of one library song. Hits come back as
    /// [`SessionEvent::QualityHit`] and do not replace the interactive search.
    pub async fn quality_search(&self, query: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::QualitySearch { query })
            .await;
    }

    /// Drops finished downloads, finished uploads, or both.
    pub async fn clear_finished(&self, downloads: bool, uploads: bool) {
        let _ = self
            .commands
            .send(peers::SessionCommand::ClearFinished { downloads, uploads })
            .await;
    }

    /// Drops a queued or running transfer. A waiting upload is answered with `UploadDenied` `Cancelled`.
    pub async fn cancel(&self, direction: crate::model::Direction, user: String, path: String) {
        let _ = self
            .commands
            .send(peers::SessionCommand::Cancel {
                direction,
                user,
                path,
            })
            .await;
    }

    /// Asks the server for `username` and opens a peer socket of `conn_type`.
    pub async fn connect_peer(&self, username: impl Into<String>, conn_type: ConnType) {
        let _ = self
            .commands
            .send(SessionCommand::ConnectPeer {
                username: username.into(),
                conn_type,
            })
            .await;
    }

    pub(crate) fn start(
        config: Config,
        events: mpsc::Sender<SessionEvent>,
        timeout: Duration,
        ping_interval: Duration,
    ) -> Self {
        Self::start_with_cap(config, events, timeout, ping_interval, MAX_PEERS)
    }

    pub(crate) fn start_with_cap(
        config: Config,
        events: mpsc::Sender<SessionEvent>,
        timeout: Duration,
        ping_interval: Duration,
        peer_cap: usize,
    ) -> Self {
        let (shutdown, mut flag) = watch::channel(false);
        let (commands, commands_rx) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = flag.changed() => {}
                outcome = login(&config, timeout) => {
                    match outcome {
                        Ok((link, SessionEvent::LoggedIn { banner, ip, supporter })) => {
                            let _ = events
                                .send(SessionEvent::LoggedIn {
                                    banner,
                                    ip,
                                    supporter,
                                })
                                .await;
                            serve(
                                link,
                                &config,
                                events,
                                flag,
                                ping_interval,
                                commands_rx,
                                peer_cap,
                            )
                            .await;
                        }
                        Ok((link, event)) => {
                            let _ = events.send(event).await;
                            let _ = flag.changed().await;
                            drop(link);
                        }
                        Err(event) => {
                            let _ = events.send(event).await;
                        }
                    }
                }
            }
        });
        Self {
            shutdown,
            commands,
            task: Some(task),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct ServerLink {
    stream: TcpStream,
    decoder: FrameDecoder,
    last_write: Instant,
    /// False after the server socket has closed. Further writes would be `EPIPE`.
    live: bool,
}

async fn login(
    config: &Config,
    timeout: Duration,
) -> Result<(ServerLink, SessionEvent), SessionEvent> {
    let frame = ClientMessage::Login(LoginRequest::new(&config.username, config.password()))
        .frame()
        .map_err(protocol_event)?;
    let deadline = Instant::now() + timeout;
    let mut stream = match deadline_io(
        deadline,
        TcpStream::connect((config.server_host.as_str(), config.server_port)),
    )
    .await
    {
        Ok(stream) => stream,
        Err(event) => return Err(event),
    };
    deadline_io(deadline, stream.write_all(&frame)).await?;
    let mut link = ServerLink {
        stream,
        decoder: FrameDecoder::new(),
        last_write: Instant::now(),
        live: true,
    };
    let frame = read_frame(&mut link, deadline).await?;
    if frame.code != server::LOGIN {
        return Err(SessionEvent::Disconnected {
            message: format!(
                "expected login reply code {}, got {}",
                server::LOGIN,
                frame.code
            ),
        });
    }
    let event = match decode_login_response(&frame.payload) {
        Ok(LoginResponse::Success {
            banner,
            ip,
            supporter,
            ..
        }) => SessionEvent::LoggedIn {
            banner,
            ip,
            supporter,
        },
        Ok(LoginResponse::Failure { reason }) => SessionEvent::LoginFailed { reason },
        Err(err) => return Err(protocol_event(err)),
    };
    Ok((link, event))
}

/// Binds exactly `config.listen_port`. The port range is one entry, so there is no fallback port.
async fn bind_listen(config: &Config) -> Result<TcpListener, SessionEvent> {
    let ip: IpAddr = if config.bind_address.is_empty() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        config
            .bind_address
            .parse()
            .map_err(|_| SessionEvent::ListenFailed {
                message: format!("bind_address {} is not an IP address", config.bind_address),
            })?
    };
    let addr = SocketAddr::new(ip, config.listen_port);
    TcpListener::bind(addr)
        .await
        .map_err(|err| SessionEvent::ListenFailed {
            message: format!("listen port {}: {err}", config.listen_port),
        })
}

/// A specific listen address is that interface. An unspecified bind uses the address of the server connection.
fn interface_in_use(listener: &TcpListener, link: &ServerLink) -> String {
    let bound = listener.local_addr().ok().map(|addr| addr.ip());
    let server = link.stream.local_addr().ok().map(|addr| addr.ip());
    chosen_interface(bound, server)
}

fn chosen_interface(bound: Option<IpAddr>, server: Option<IpAddr>) -> String {
    match bound {
        Some(ip) if !ip.is_unspecified() => ip.to_string(),
        _ => server
            .filter(|ip| !ip.is_unspecified())
            .map(|ip| ip.to_string())
            .unwrap_or_default(),
    }
}

async fn publish_public_shares(
    link: &mut ServerLink,
    search: &search::SearchState,
) -> Result<(), SessionEvent> {
    let (folders, files) = search.public_counts();
    send_message(link, &ClientMessage::SharedFoldersFiles { folders, files }).await
}

async fn publish_moved(
    moved: &transfers::MovedFile,
    share_roots: &mut crate::config::Shares,
    search: &mut search::SearchState,
    transfers: &mut transfers::Book,
    link: &mut ServerLink,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    let added = search.store_download(share_roots, &moved.root, &moved.from, &moved.to);
    if !moved.swept.is_empty() {
        search.forget(&moved.swept);
    }
    transfers.set_shared(transfers::shared_files(search.index()));
    let _ = events.send(share_counts(search.index())).await;
    if let Some(path) = added {
        let _ = events.send(SessionEvent::ShareRoot { path }).await;
    }
    if let Err(event) = publish_public_shares(link, search).await {
        let _ = events.send(event).await;
        return false;
    }
    true
}

fn share_reply(
    username: &str,
    code: u32,
    payload: &[u8],
    search: &search::SearchState,
    transfers: &transfers::Book,
) -> Option<Vec<u8>> {
    if code == crate::protocol::peer::SHARED_FILE_LIST_REQUEST {
        let body = search.share_list(username).encode().ok()?;
        return crate::protocol::encode_frame(
            crate::protocol::peer::SHARED_FILE_LIST_RESPONSE,
            &body,
        )
        .ok();
    }
    if code == crate::protocol::peer::USER_INFO_REQUEST {
        let (queue_size, slots_available) = transfers.user_info_queue();
        let info = crate::protocol::peer::UserInfoResponse {
            description: String::new(),
            picture: None,
            total_uploads: 0,
            queue_size,
            slots_available,
            upload_allowed: 0,
        };
        return crate::protocol::peer::encode_user_info_response(&info).ok();
    }
    if code == crate::protocol::peer::FOLDER_CONTENTS_REQUEST {
        let request = crate::protocol::peer::decode_folder_contents_request(payload).ok()?;
        let response = crate::protocol::peer::FolderContentsResponse {
            token: request.token,
            directory: request.directory.clone(),
            folders: search.folder_for(username, &request.directory),
        };
        return crate::protocol::peer::encode_folder_contents_response(&response).ok();
    }
    None
}

fn share_counts(index: &crate::shares::ShareIndex) -> SessionEvent {
    let [public, buddy, trusted] = index.counts();
    SessionEvent::Shares {
        public_files: public.files,
        public_folders: public.folders,
        buddy_files: buddy.files,
        buddy_folders: buddy.folders,
        trusted_files: trusted.files,
        trusted_folders: trusted.folders,
    }
}

fn portmap_gateway(config: &Config) -> Result<Option<SocketAddr>, String> {
    if config.upnp_gateway.is_empty() {
        Ok(None)
    } else {
        config
            .upnp_gateway
            .parse()
            .map(Some)
            .map_err(|_| format!("upnp_gateway {} is not host:port", config.upnp_gateway))
    }
}

async fn announce(link: &mut ServerLink, port: u16) -> Result<(), SessionEvent> {
    send_message(link, &ClientMessage::SetWaitPort(u32::from(port))).await?;
    send_message(
        link,
        &ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        }),
    )
    .await
}

fn lost_server(err: &io::Error) -> SessionEvent {
    SessionEvent::Disconnected {
        message: format!("{err}. reconnecting"),
    }
}

async fn send_frame(link: &mut ServerLink, frame: Vec<u8>) -> Result<(), SessionEvent> {
    if !link.live {
        return Ok(());
    }
    if let Err(err) = link.stream.write_all(&frame).await {
        link.live = false;
        return Err(lost_server(&err));
    }
    link.last_write = Instant::now();
    Ok(())
}

async fn read_server(up: bool, stream: &mut TcpStream, buf: &mut [u8]) -> io::Result<usize> {
    if !up {
        std::future::pending().await
    } else {
        stream.read(buf).await
    }
}

async fn poll_join<T>(handle: &mut JoinHandle<T>) -> Result<T, tokio::task::JoinError> {
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *handle).poll(cx)).await
}

/// Puts the listen port, rooms, shares, and queued downloads back on a new server socket.
async fn resume_server(
    link: &mut ServerLink,
    config: &Config,
    listening: bool,
    book: &mut PeerBook,
    transfers: &mut transfers::Book,
    search: &search::SearchState,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    if listening && announce(link, config.listen_port).await.is_err() {
        return false;
    }
    if listening {
        for buddy in &config.buddies {
            if send_message(link, &ClientMessage::WatchUser(buddy.name.clone()))
                .await
                .is_err()
            {
                return false;
            }
        }
        for room in &config.auto_join {
            let Ok(frame) = crate::protocol::chat::encode_join_room(room, false) else {
                return false;
            };
            if send_frame(link, frame).await.is_err() {
                return false;
            }
        }
    }
    if publish_public_shares(link, search).await.is_err() {
        return false;
    }
    for user in transfers.queued_download_users() {
        if !ask_peer(link, book, events, transfers, &user, ConnType::Peer).await {
            return false;
        }
    }
    true
}

async fn send_message(link: &mut ServerLink, message: &ClientMessage) -> Result<(), SessionEvent> {
    if !link.live {
        return Ok(());
    }
    let frame = message.frame().map_err(protocol_event)?;
    if let Err(err) = link.stream.write_all(&frame).await {
        link.live = false;
        return Err(lost_server(&err));
    }
    link.last_write = Instant::now();
    Ok(())
}

struct PeerTasks {
    handles: Vec<JoinHandle<()>>,
}

impl Drop for PeerTasks {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

enum PeerReport {
    Direct {
        username: String,
        conn_type: ConnType,
        stream: Result<TcpStream, ()>,
    },
    Incoming {
        stream: TcpStream,
        handshake: Result<(PeerHandshake, Vec<u8>), ProtocolError>,
    },
    Pierce {
        username: String,
        conn_type: ConnType,
        token: u32,
        stream: Result<TcpStream, ()>,
    },
    Held(TcpStream),
}

enum ServerAction {
    PeerAddress(PeerAddress),
    ConnectToPeer(IncomingConnect),
    CantConnect {
        token: u32,
    },
    IncomingSearch {
        username: String,
        token: u32,
        query: String,
    },
    WishlistInterval,
    PossibleParents(Vec<crate::protocol::PossibleParent>),
    UserFlag {
        user: String,
        privileged: bool,
    },
    PeerOffline(String),
    SendFrame(Vec<u8>),
}

async fn serve(
    mut link: ServerLink,
    config: &Config,
    events: mpsc::Sender<SessionEvent>,
    mut flag: watch::Receiver<bool>,
    ping_interval: Duration,
    mut commands: mpsc::Receiver<SessionCommand>,
    peer_cap: usize,
) {
    let mut listener = match bind_listen(config).await {
        Ok(listener) => Some(listener),
        Err(event) => {
            let _ = events.send(event).await;
            None
        }
    };
    if listener.is_some() {
        if let Err(event) = announce(&mut link, config.listen_port).await {
            let _ = events.send(event).await;
            return;
        }
        let address = listener
            .as_ref()
            .map(|socket| interface_in_use(socket, &link))
            .unwrap_or_default();
        let _ = events
            .send(SessionEvent::Listening {
                port: config.listen_port,
                address,
            })
            .await;
        for buddy in &config.buddies {
            if send_message(&mut link, &ClientMessage::WatchUser(buddy.name.clone()))
                .await
                .is_err()
            {
                return;
            }
        }
        for room in &config.auto_join {
            let frame = match crate::protocol::chat::encode_join_room(room, false) {
                Ok(frame) => frame,
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return;
                }
            };
            if send_frame(&mut link, frame).await.is_err() {
                return;
            }
        }
    }
    let mut book = PeerBook::new(peer_cap);
    let mut browsing: HashSet<String> = HashSet::new();
    let mut folders: HashMap<String, Vec<(u32, String)>> = HashMap::new();
    let mut folder_token: u32 = 0;
    let mut tasks = PeerTasks {
        handles: Vec::new(),
    };
    if listener.is_some() && config.upnp {
        match portmap_gateway(config) {
            Ok(gateway) => {
                let port = config.listen_port;
                let mapped_events = events.clone();
                let mapped_flag = flag.clone();
                tasks.handles.push(tokio::spawn(async move {
                    portmap::open_mapping(gateway, port, mapped_events, mapped_flag).await;
                }));
            }
            Err(message) => {
                let _ = events.send(SessionEvent::PortMapFailed { message }).await;
            }
        }
    }
    let mut held: Vec<TcpStream> = Vec::new();
    let mut peer_writers: HashMap<String, OwnedWriteHalf> = HashMap::new();
    let mut peer_generation: HashMap<String, u64> = HashMap::new();
    let mut peer_gen: u64 = 0;
    let mut idle_writers: Vec<OwnedWriteHalf> = Vec::new();
    let (report_tx, mut reports) = mpsc::channel(64);
    let (frame_tx, mut peer_frames) = mpsc::channel(64);
    let (distrib_tx, mut distrib_in) = mpsc::channel(32);
    let (progress_tx, mut progress_rx) = mpsc::channel(32);
    let (file_tx, mut file_rx) = mpsc::channel(32);
    let mut parked: Vec<ParkedFile> = Vec::new();
    let mut sweep = Instant::now() + CONNECT_TIMEOUT;
    let mut retry_at = Instant::now() + RETRY_CONNECTION;
    let index = match crate::shares::rescan(&config.shares).await {
        Ok(index) => index,
        Err(error) => {
            let _ = events
                .send(SessionEvent::ShareScanFailed {
                    message: error.to_string(),
                })
                .await;
            crate::shares::ShareIndex::empty()
        }
    };
    let _ = events.send(share_counts(&index)).await;
    let shared = transfers::shared_files(&index);
    let mut search = search::SearchState::new(config, index);
    let mut transfers = transfers::Book::new(config, shared);
    if let Some(path) = config.queue_file.clone() {
        transfers.use_queue(path);
        for transfer in transfers.snapshot() {
            let _ = events.send(SessionEvent::Transfer(transfer)).await;
        }
        for user in transfers.queued_download_users() {
            if !ask_peer(
                &mut link,
                &mut book,
                &events,
                &mut transfers,
                &user,
                ConnType::Peer,
            )
            .await
            {
                return;
            }
        }
    }
    let mut rooms = chat::RoomChat::new(config);
    let mut share_roots = config.shares.clone();
    let mut buf = [0u8; 8192];
    let mut server_up = true;
    let mut relogin = true;
    let mut reconnect_at = Instant::now();
    let mut reconnect: Option<JoinHandle<Result<(ServerLink, SessionEvent), SessionEvent>>> = None;
    let username = config.username.clone();
    if let Err(event) = publish_public_shares(&mut link, &search).await {
        let _ = events.send(event).await;
        return;
    }
    loop {
        let ping_at = link.last_write + ping_interval;
        macro_rules! server_down {
            () => {{
                server_up = false;
                link.live = false;
                relogin = true;
                if reconnect.is_none() {
                    reconnect_at = Instant::now();
                }
            }};
        }
        tokio::select! {
            changed = flag.changed() => {
                if changed.is_ok() {
                    break;
                }
            }
            _ = tokio::time::sleep_until(ping_at), if server_up => {
                if let Err(event) = send_message(&mut link, &ClientMessage::Ping).await {
                    let _ = events.send(event).await;
                    server_down!();
                }
            }
            _ = tokio::time::sleep_until(reconnect_at), if !server_up && relogin && reconnect.is_none() => {
                let config = config.clone();
                reconnect = Some(tokio::spawn(async move {
                    login(&config, CONNECT_TIMEOUT).await
                }));
            }
            joined = async {
                match reconnect.as_mut() {
                    Some(handle) => poll_join(handle).await,
                    None => std::future::pending().await,
                }
            }, if reconnect.is_some() => {
                reconnect = None;
                match joined {
                    Ok(Ok((
                        new_link,
                        SessionEvent::LoggedIn {
                            banner,
                            ip,
                            supporter,
                        },
                    ))) => {
                        link = new_link;
                        server_up = true;
                        let _ = events
                            .send(SessionEvent::LoggedIn {
                                banner,
                                ip,
                                supporter,
                            })
                            .await;
                        if !resume_server(
                            &mut link,
                            config,
                            listener.is_some(),
                            &mut book,
                            &mut transfers,
                            &search,
                            &events,
                        )
                        .await
                        {
                            server_up = false;
                            link.live = false;
                            relogin = true;
                            reconnect_at = Instant::now() + SERVER_RECONNECT;
                        }
                    }
                    Ok(Ok((dead, event))) => {
                        drop(dead);
                        let _ = events.send(event).await;
                        server_up = false;
                        link.live = false;
                        relogin = true;
                        reconnect_at = Instant::now() + SERVER_RECONNECT;
                    }
                    Ok(Err(event @ SessionEvent::LoginFailed { .. })) => {
                        relogin = false;
                        let _ = events.send(event).await;
                    }
                    Ok(Err(_)) | Err(_) => {
                        server_up = false;
                        link.live = false;
                        relogin = true;
                        reconnect_at = Instant::now() + SERVER_RECONNECT;
                    }
                }
            }
            _ = tokio::time::sleep_until(sweep) => {
                sweep = Instant::now() + CONNECT_TIMEOUT;
                recover_peer_slots(&mut book, &mut transfers, &events).await;
                let now = Instant::now();
                take_matching_files(&mut parked, &mut transfers, &mut tasks, &progress_tx);
                let waiting = std::mem::take(&mut parked);
                for file in waiting {
                    if now.duration_since(file.at) < CONNECT_TIMEOUT {
                        parked.push(file);
                        continue;
                    }
                    book.release();
                    let _ = events
                        .send(SessionEvent::PeerFailed {
                            username: file.username,
                            message: format!("no download is waiting for token {}", file.token),
                        })
                        .await;
                }
                if Instant::now() >= retry_at {
                    retry_at = Instant::now() + RETRY_CONNECTION;
                    if server_up
                        && !retry_connection_downloads(
                            &mut link,
                            &mut book,
                            &mut peer_writers,
                            &mut transfers,
                            &events,
                        )
                        .await
                    {
                        server_down!();
                    }
                }
            }
            command = commands.recv() => {
                match command {
                    Some(SessionCommand::ConnectPeer { username: peer, conn_type }) => {
                        if !ask_peer(&mut link, &mut book, &events, &mut transfers, &peer, conn_type).await {
                            server_down!();
                        }
                    }
                    Some(SessionCommand::Browse { user }) => {
                        browsing.insert(user.clone());
                        if let Some(writer) = peer_writers.get_mut(&user) {
                            if write_browse(writer).await.is_err() {
                                continue;
                            }
                        } else if !ask_peer(
                            &mut link,
                            &mut book,
                            &events,
                            &mut transfers,
                            &user,
                            ConnType::Peer,
                        )
                        .await
                        {
                            server_down!();
                        }
                    }
                    Some(SessionCommand::Folder { user, directory }) => {
                        folder_token = folder_token.wrapping_add(1).max(1);
                        let token = folder_token;
                        if let Some(writer) = peer_writers.get_mut(&user) {
                            if write_folder(writer, token, &directory).await.is_err() {
                                continue;
                            }
                        } else {
                            let first =
                                !folders.contains_key(&user) && !browsing.contains(&user);
                            folders.entry(user.clone()).or_default().push((token, directory));
                            if first
                                && !ask_peer(
                                    &mut link,
                                    &mut book,
                                    &events,
                                    &mut transfers,
                                    &user,
                                    ConnType::Peer,
                                )
                                .await
                            {
                                server_down!();
                            }
                        }
                    }
                    Some(SessionCommand::Say { room, text, ack }) => {
                        let spoken = rooms.outgoing(&text);
                        match crate::protocol::chat::encode_say_chatroom(&room, &spoken) {
                            Ok(frame) => {
                                if send_frame(&mut link, frame).await.is_err() {
                                    server_down!();
                                }
                            }
                            Err(err) => {
                                let _ = events.send(protocol_event(err)).await;
                            }
                        }
                        let _ = ack.send(());
                    }
                    Some(SessionCommand::Inspect { user }) => {
                        let messages = [
                            ClientMessage::WatchUser(user.clone()),
                            ClientMessage::GetUserStats(user.clone()),
                            ClientMessage::UserInterests(user),
                        ];
                        let mut failed = false;
                        for message in messages {
                            if send_message(&mut link, &message).await.is_err() {
                                failed = true;
                                break;
                            }
                        }
                        if failed {
                            server_down!();
                        }
                    }
                    Some(SessionCommand::Join { room }) => {
                        match crate::protocol::chat::encode_join_room(&room, false) {
                            Ok(frame) => {
                                if send_frame(&mut link, frame).await.is_err() {
                                    server_down!();
                                }
                            }
                            Err(err) => {
                                let _ = events.send(protocol_event(err)).await;
                            }
                        }
                    }
                    Some(SessionCommand::SetAway { away, ack }) => {
                        rooms.set_away(away);
                        let status = if away {
                            UserStatus::Away
                        } else {
                            UserStatus::Online
                        };
                        if send_message(&mut link, &ClientMessage::SetStatus(SetStatus { status }))
                            .await
                            .is_err()
                        {
                            server_down!();
                        }
                        let _ = ack.send(());
                    }
                    Some(SessionCommand::SetLists {
                        banned,
                        ignored,
                        prioritized,
                        ack,
                    }) => {
                        transfers.set_blocks(&banned, &ignored, &prioritized);
                        let _ = ack.send(());
                    }
                    Some(SessionCommand::AlbumUnavailable { user, album }) => {
                        emit_transfers(&events, transfers.mark_unavailable(&user, &album)).await;
                    }
                    Some(SessionCommand::FileUnavailable { user, path }) => {
                        emit_transfers(&events, transfers.mark_file_unavailable(&user, &path))
                            .await;
                    }
                    Some(SessionCommand::Download {
                        user,
                        path,
                        size,
                        folder,
                        stage,
                    }) => {
                        match transfers.enqueue_download(&transfers::DownloadRequest {
                            user: user.clone(),
                            path,
                            size,
                            folder,
                            stage,
                        }) {
                            Ok(view) => {
                                let _ = events.send(SessionEvent::Transfer(view)).await;
                                if let Some(writer) = peer_writers.get_mut(&user) {
                                    if !write_download_asks(writer, &mut transfers, &user).await {
                                        peer_writers.remove(&user);
                                    }
                                } else if !book.connecting(&user, ConnType::Peer) && !ask_peer(
                                    &mut link,
                                    &mut book,
                                    &events,
                                    &mut transfers,
                                    &user,
                                    ConnType::Peer,
                                )
                                .await
                                {
                                    server_down!();
                                }
                            }
                            Err(message) => {
                                let _ = events.send(SessionEvent::ShareScanFailed { message }).await;
                            }
                        }
                    }
                    Some(SessionCommand::ClearFinished {
                        downloads,
                        uploads,
                    }) => {
                        transfers.clear_finished(downloads, uploads);
                    }
                    Some(SessionCommand::Cancel {
                        direction,
                        user,
                        path,
                    }) => {
                        let outs = transfers.cancel(direction, &user, &path);
                        if !apply_outs(
                            outs,
                            &mut link,
                            &mut book,
                            &mut peer_writers,
                            &mut transfers,
                            &events,
                        )
                        .await
                        {
                            server_down!();
                        }
                    }
                    Some(SessionCommand::ApplyPrefs { prefs }) => {
                        transfers.apply_prefs(&prefs);
                        search.apply_prefs(&prefs);
                        rooms.apply_prefs(&prefs);
                    }
                    Some(SessionCommand::Forget { paths }) => {
                        search.forget(&paths);
                        transfers.set_shared(transfers::shared_files(search.index()));
                        let _ = events.send(share_counts(search.index())).await;
                        if let Err(event) = publish_public_shares(&mut link, &search).await {
                            let _ = events.send(event).await;
                            server_down!();
                        }
                    }
                    Some(SessionCommand::Relocate { moves }) => {
                        let mut failed = false;
                        for relocation in moves {
                            let moved = transfers::MovedFile {
                                root: relocation.root,
                                from: relocation.from,
                                to: relocation.to,
                                swept: Vec::new(),
                            };
                            if !publish_moved(
                                &moved,
                                &mut share_roots,
                                &mut search,
                                &mut transfers,
                                &mut link,
                                &events,
                            )
                            .await
                            {
                                failed = true;
                                break;
                            }
                        }
                        if failed {
                            server_down!();
                        }
                    }
                    Some(SessionCommand::Rescan { shares }) => {
                        match crate::shares::rescan(&shares).await {
                            Ok(index) => {
                                share_roots = shares;
                                let event = share_counts(&index);
                                transfers.set_shared(transfers::shared_files(&index));
                                search.set_index(index);
                                let _ = events.send(event).await;
                                if let Err(event) = publish_public_shares(&mut link, &search).await {
                                    let _ = events.send(event).await;
                                    server_down!();
                                }
                            }
                            Err(error) => {
                                let _ = events
                                    .send(SessionEvent::ShareScanFailed {
                                        message: error.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    Some(SessionCommand::QualitySearch { query }) => {
                        let request = SearchRequest {
                            mode: crate::model::SearchMode::Global,
                            query,
                            room: String::new(),
                            user: String::new(),
                        };
                        let token = search.begin_probe();
                        match search::outbound(&request, token, search.buddies()) {
                            Ok(messages) => {
                                let mut failed = false;
                                for message in messages {
                                    if let Err(event) = send_message(&mut link, &message).await {
                                        let _ = events.send(event).await;
                                        failed = true;
                                        break;
                                    }
                                }
                                if failed {
                                    server_down!();
                                }
                            }
                            Err(_) => search.end_probe(),
                        }
                    }
                    Some(SessionCommand::Search { mode, query, room, user }) => {
                        let request = SearchRequest { mode, query, room, user };
                        let token = search.begin_search();
                        match search::outbound(&request, token, search.buddies()) {
                            Ok(messages) => {
                                let _ = events.send(SessionEvent::SearchBegan(token)).await;
                                let mut failed = false;
                                for message in messages {
                                    if let Err(event) = send_message(&mut link, &message).await {
                                        let _ = events.send(event).await;
                                        failed = true;
                                        break;
                                    }
                                }
                                if failed {
                                    server_down!();
                                }
                            }
                            Err(message) => {
                                search.cancel_search();
                                let _ = events.send(SessionEvent::ShareScanFailed {
                                    message: message.to_owned(),
                                }).await;
                            }
                        }
                    }
                    None => break,
                }
            }
            read = read_server(server_up, &mut link.stream, &mut buf) => {
                match read {
                    Ok(0) => {
                        let _ = events
                            .send(SessionEvent::Disconnected {
                                message: "server closed the connection. reconnecting".to_owned(),
                            })
                            .await;
                        server_down!();
                    }
                    Ok(count) => {
                        link.decoder.push(&buf[..count]);
                        match dispatch(&mut link.decoder, &events, &mut rooms).await {
                            Ok(actions) => {
                                if !apply_server_actions(&mut link, &events, &mut book, &mut tasks, &report_tx, &username, listener.is_some(), &mut search, &mut transfers, actions).await {
                                    server_down!();
                                }
                            }
                            Err(()) => break,
                        }
                    }
                    Err(err) => {
                        let _ = events
                            .send(SessionEvent::Disconnected {
                                message: format!("{err}. reconnecting"),
                            })
                            .await;
                        server_down!();
                    }
                }
            }
            accepted = accept_peer(&mut listener) => {
                match accepted {
                    Ok((stream, _)) => {
                        let report_tx = report_tx.clone();
                        tasks.handles.push(tokio::spawn(async move {
                            let mut stream = stream;
                            let handshake = peers::read_handshake(&mut stream).await;
                            let _ = report_tx
                                .send(PeerReport::Incoming { stream, handshake })
                                .await;
                        }));
                    }
                    Err(err) => {
                        let _ = events
                            .send(SessionEvent::Disconnected {
                                message: format!("listen socket: {err}. reconnecting"),
                            })
                            .await;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
            report = reports.recv() => {
                let Some(report) = report else { break };
                if !apply_peer_report(
                    report,
                    &mut link,
                    &events,
                    &mut book,
                    &mut held,
                    &mut peer_writers,
                    &mut idle_writers,
                    &mut tasks,
                    &frame_tx,
                    &distrib_tx,
                    &mut search,
                    &mut transfers,
                    &progress_tx,
                    &file_tx,
                    listener.is_some(),
                    &browsing,
                    &mut folders,
                    &mut peer_gen,
                    &mut peer_generation,
                )
                .await
                {
                    server_down!();
                }
            }
            frame = peer_frames.recv() => {
                let Some(incoming) = frame else { break };
                let (username, peer_frame) = match incoming {
                    search::PeerIn::Closed {
                        username,
                        generation,
                    } => {
                        if !forget_peer(
                            &username,
                            generation,
                            &mut peer_generation,
                            &mut peer_writers,
                            &mut book,
                            &mut transfers,
                            &mut link,
                            &events,
                        )
                        .await
                        {
                            server_down!();
                        }
                        continue;
                    }
                    search::PeerIn::Frame { username, frame } => (username, frame),
                };
                if peer_frame.code == crate::protocol::peer::SHARED_FILE_LIST_REQUEST {
                    let _ = events
                        .send(SessionEvent::Log(format!(
                            "User {username} is browsing your list of shared files"
                        )))
                        .await;
                }
                if let Some(reply) = share_reply(
                    &username,
                    peer_frame.code,
                    &peer_frame.payload,
                    &search,
                    &transfers,
                ) && let Some(writer) = peer_writers.get_mut(&username)
                {
                    let _ = writer.write_all(&reply).await;
                }
                if peer_frame.code == crate::protocol::peer::FILE_SEARCH_RESPONSE
                    && let Ok(response) = FileSearchResponse::decode(&peer_frame.payload)
                    && search.accepts(response.token)
                {
                    let probe = search.is_probe(response.token);
                    for hit in search::hits_from(&response) {
                        if transfers.blocks(&hit.user) {
                            continue;
                        }
                        let event = if probe {
                            SessionEvent::QualityHit(hit)
                        } else {
                            SessionEvent::SearchResult(response.token, hit)
                        };
                        let _ = events.send(event).await;
                    }
                }
                if peer_frame.code == crate::protocol::peer::SHARED_FILE_LIST_RESPONSE
                    && let Ok(response) = crate::protocol::peer::SharedFileListResponse::decode(
                        &peer_frame.payload,
                    )
                {
                    let _ = events
                        .send(SessionEvent::Browse {
                            user: username.clone(),
                            rows: browse::rows_from_folders(&response.list),
                        })
                        .await;
                }
                if peer_frame.code == crate::protocol::peer::FOLDER_CONTENTS_RESPONSE
                    && let Ok(response) = crate::protocol::peer::decode_folder_contents_response(
                        &peer_frame.payload,
                    )
                {
                    let _ = events
                        .send(SessionEvent::Folder {
                            user: username.clone(),
                            directory: response.directory.replace('/', "\\"),
                            files: browse::hits_from_folders(&username, &response.folders),
                        })
                        .await;
                }
                if peer_frame.code == crate::protocol::peer::USER_INFO_RESPONSE
                    && let Ok(info) =
                        crate::protocol::peer::decode_user_info_response(&peer_frame.payload)
                {
                    let _ = events
                        .send(SessionEvent::UserInfo {
                            user: username.clone(),
                            description: info.description,
                            total_uploads: info.total_uploads,
                            queue_size: info.queue_size,
                            slots_available: info.slots_available,
                        })
                        .await;
                }
                let outs = transfers.on_peer(&username, peer_frame.code, &peer_frame.payload);
                if !apply_outs(
                    outs,
                    &mut link,
                    &mut book,
                    &mut peer_writers,
                    &mut transfers,
                    &events,
                )
                .await
                {
                    server_down!();
                }
                take_matching_files(&mut parked, &mut transfers, &mut tasks, &progress_tx);
            }
            offer = file_rx.recv() => {
                let Some(offer) = offer else { break };
                match offer {
                    FileIn::Failed { username, message } => {
                        book.release();
                        let _ = events
                            .send(SessionEvent::PeerFailed { username, message })
                            .await;
                    }
                    FileIn::Ready {
                        username,
                        token,
                        stream,
                        prefix,
                    } => {
                        if let Some(job) = transfers.download_for_token(token) {
                            begin_receive(
                                job,
                                stream,
                                prefix,
                                &transfers,
                                &mut tasks,
                                &progress_tx,
                            );
                        } else {
                            parked.push(ParkedFile {
                                username,
                                token,
                                stream,
                                prefix,
                                at: Instant::now(),
                            });
                        }
                    }
                }
            }
            progress = progress_rx.recv() => {
                let Some(progress) = progress else { break };
                let socket_done = progress.state != TransferState::Transferring;
                if progress.direction == Direction::Upload
                    && progress.state == TransferState::ConnectionClosed
                    && let Some(frame) = transfers::upload_failed_frame(&progress.path)
                    && let Some(writer) = peer_writers.get_mut(&progress.user)
                {
                    let _ = writer.write_all(&frame).await;
                }
                let moved = progress.moved.clone();
                let outs = transfers.on_progress(progress);
                if !apply_outs(
                    outs,
                    &mut link,
                    &mut book,
                    &mut peer_writers,
                    &mut transfers,
                    &events,
                )
                .await
                {
                    server_down!();
                }
                if socket_done {
                    book.release();
                }
                if let Some(moved) = moved
                    && !publish_moved(
                        &moved,
                        &mut share_roots,
                        &mut search,
                        &mut transfers,
                        &mut link,
                        &events,
                    )
                    .await
                {
                    server_down!();
                }
            }
            incoming = distrib_in.recv() => {
                let Some(incoming) = incoming else { break };
                let from_parent = incoming.from_parent;
                let Some(message) = incoming.message else {
                    book.release();
                    if from_parent {
                        search.parent = None;
                        search.pending_parent = None;
                    } else {
                        search.child_count = search.child_count.saturating_sub(1);
                    }
                    let _ = events
                        .send(SessionEvent::Network {
                            parent: search.parent.clone(),
                            children: u32::try_from(search.child_count).unwrap_or(u32::MAX),
                        })
                        .await;
                    continue;
                };
                if from_parent {
                    search::relay_search(&mut search.children, &message).await;
                }
                if let crate::protocol::DistribMessage::Search {
                    username: searcher,
                    token,
                    query,
                    ..
                } = message
                    && let Some((frame, results)) = search.answer(token, &query, &username)
                {
                    let _ = events
                        .send(SessionEvent::Log(format!(
                            "User {searcher} is searching for \"{query}\", found {results} results"
                        )))
                        .await;
                    search.queue_reply(searcher.clone(), frame);
                    if let Err(event) = send_message(
                        &mut link,
                        &ClientMessage::GetPeerAddress(searcher),
                    )
                    .await
                    {
                        let _ = events.send(event).await;
                        server_down!();
                    }
                }
            }
        }
    }
}

async fn accept_peer(listener: &mut Option<TcpListener>) -> io::Result<(TcpStream, SocketAddr)> {
    match listener.as_mut() {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_server_actions(
    link: &mut ServerLink,
    events: &mpsc::Sender<SessionEvent>,
    book: &mut PeerBook,
    tasks: &mut PeerTasks,
    report_tx: &mpsc::Sender<PeerReport>,
    our_username: &str,
    listening: bool,
    search: &mut search::SearchState,
    transfers: &mut transfers::Book,
    actions: Vec<ServerAction>,
) -> bool {
    for action in actions {
        match action {
            ServerAction::PeerAddress(address) => {
                let waited = book.take_wait(&address.username);
                let Some(conn_type) = waited else {
                    if let Some(frame) = search.take_reply(&address.username) {
                        let Some(addr) = peer_addr(address.ip, address.port) else {
                            continue;
                        };
                        let report_tx = report_tx.clone();
                        let our_username = our_username.to_owned();
                        tasks.handles.push(tokio::spawn(async move {
                            if let Ok(mut stream) =
                                peers::dial_direct(addr, &our_username, ConnType::Peer).await
                            {
                                let _ = stream.write_all(&frame).await;
                                let _ = report_tx.send(PeerReport::Held(stream)).await;
                            }
                        }));
                    }
                    continue;
                };
                let username = address.username.clone();
                let Some(addr) = peer_addr(address.ip, address.port) else {
                    if !fallback_indirect(link, events, book, &username, conn_type, listening).await
                    {
                        return false;
                    }
                    continue;
                };
                let report_tx = report_tx.clone();
                let our_username = our_username.to_owned();
                tasks.handles.push(tokio::spawn(async move {
                    let stream = peers::dial_direct(addr, &our_username, conn_type).await;
                    let _ = report_tx
                        .send(PeerReport::Direct {
                            username,
                            conn_type,
                            stream,
                        })
                        .await;
                }));
            }
            ServerAction::CantConnect { token } => {
                let Some((username, conn_type)) = book.take_indirect(token) else {
                    continue;
                };
                book.release();
                if conn_type == ConnType::Peer {
                    emit_transfers(
                        events,
                        transfers.fail_queued(&username, TransferState::ConnectionClosed),
                    )
                    .await;
                }
            }
            ServerAction::ConnectToPeer(request) => {
                if !book.try_reserve() {
                    recover_peer_slots(book, transfers, events).await;
                    if !book.try_reserve() {
                        let _ = events
                            .send(SessionEvent::PeerFailed {
                                username: request.username,
                                message: "peer connection cap reached".to_owned(),
                            })
                            .await;
                        continue;
                    }
                }
                let Some(addr) = peer_addr(request.ip, request.port) else {
                    book.release();
                    let _ = events
                        .send(SessionEvent::PeerFailed {
                            username: request.username,
                            message: "peer address has no port".to_owned(),
                        })
                        .await;
                    continue;
                };
                let report_tx = report_tx.clone();
                tasks.handles.push(tokio::spawn(async move {
                    let stream = peers::dial_pierce(addr, request.token).await;
                    let _ = report_tx
                        .send(PeerReport::Pierce {
                            username: request.username,
                            conn_type: request.conn_type,
                            token: request.token,
                            stream,
                        })
                        .await;
                }));
            }
            ServerAction::IncomingSearch {
                username,
                token,
                query,
            } => {
                if let Some((frame, results)) = search.answer(token, &query, our_username) {
                    let _ = events
                        .send(SessionEvent::Log(format!(
                            "User {username} is searching for \"{query}\", found {results} results"
                        )))
                        .await;
                    search.queue_reply(username.clone(), frame);
                    if let Err(event) =
                        send_message(link, &ClientMessage::GetPeerAddress(username)).await
                    {
                        let _ = events.send(event).await;
                        return false;
                    }
                }
            }
            ServerAction::WishlistInterval => {
                if let Some(query) = search.rotate_wishlist() {
                    let token = search.next_token();
                    if let Some(message) = search::wishlist_message(&query, token)
                        && let Err(event) = send_message(link, &message).await
                    {
                        let _ = events.send(event).await;
                        return false;
                    }
                }
            }
            ServerAction::PossibleParents(parents) => {
                if search.parent.is_some() {
                    continue;
                }
                let Some(parent) = parents.into_iter().next() else {
                    continue;
                };
                if !book.try_reserve() {
                    recover_peer_slots(book, transfers, events).await;
                    if !book.try_reserve() {
                        continue;
                    }
                }
                search.pending_parent = Some(parent.username.clone());
                book.push_wait(parent.username.clone(), ConnType::Distributed);
                if let Err(event) =
                    send_message(link, &ClientMessage::GetPeerAddress(parent.username)).await
                {
                    let _ = events.send(event).await;
                    return false;
                }
            }
            ServerAction::UserFlag { user, privileged } => {
                transfers.set_privileged(&user, privileged);
            }
            ServerAction::PeerOffline(user) => {
                emit_transfers(
                    events,
                    transfers.fail_queued(&user, TransferState::UserLoggedOff),
                )
                .await;
            }
            ServerAction::SendFrame(frame) => {
                if send_frame(link, frame).await.is_err() {
                    return false;
                }
            }
        }
    }
    true
}

enum FileIn {
    Ready {
        username: String,
        token: u32,
        stream: TcpStream,
        prefix: Vec<u8>,
    },
    Failed {
        username: String,
        message: String,
    },
}

struct ParkedFile {
    username: String,
    token: u32,
    stream: TcpStream,
    prefix: Vec<u8>,
    at: Instant,
}

fn spawn_file_read(
    stream: TcpStream,
    leftover: Vec<u8>,
    username: String,
    tasks: &mut PeerTasks,
    file_tx: &mpsc::Sender<FileIn>,
) {
    let tx = file_tx.clone();
    tasks.handles.push(tokio::spawn(async move {
        let mut stream = stream;
        let mut leftover = leftover;
        let read = tokio::time::timeout(
            Duration::from_secs(2),
            transfers::read_file_token(&mut stream, &mut leftover),
        )
        .await;
        let message = match read {
            Ok(Ok(token)) => {
                let mut prefix = token.to_le_bytes().to_vec();
                prefix.append(&mut leftover);
                FileIn::Ready {
                    username,
                    token,
                    stream,
                    prefix,
                }
            }
            _ => FileIn::Failed {
                username,
                message: "file connection closed before the token".to_owned(),
            },
        };
        let _ = tx.send(message).await;
    }));
}

fn begin_receive(
    job: transfers::FileJob,
    stream: TcpStream,
    prefix: Vec<u8>,
    transfers: &transfers::Book,
    tasks: &mut PeerTasks,
    progress_tx: &mpsc::Sender<transfers::Progress>,
) {
    let limit = transfers.download_limit();
    let tx = progress_tx.clone();
    tasks.handles.push(tokio::spawn(async move {
        let progress = transfers::receive_download(stream, prefix, job, limit, &tx).await;
        let _ = tx.send(progress).await;
    }));
}

fn take_matching_files(
    parked: &mut Vec<ParkedFile>,
    transfers: &mut transfers::Book,
    tasks: &mut PeerTasks,
    progress_tx: &mpsc::Sender<transfers::Progress>,
) {
    let waiting = std::mem::take(parked);
    for file in waiting {
        if let Some(job) = transfers.download_for_token(file.token) {
            begin_receive(job, file.stream, file.prefix, transfers, tasks, progress_tx);
        } else {
            parked.push(file);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_peer_report(
    report: PeerReport,
    link: &mut ServerLink,
    events: &mpsc::Sender<SessionEvent>,
    book: &mut PeerBook,
    held: &mut Vec<TcpStream>,
    peer_writers: &mut HashMap<String, OwnedWriteHalf>,
    idle_writers: &mut Vec<OwnedWriteHalf>,
    tasks: &mut PeerTasks,
    frame_tx: &mpsc::Sender<search::PeerIn>,
    distrib_tx: &mpsc::Sender<search::DistribIn>,
    search_state: &mut search::SearchState,
    transfers: &mut transfers::Book,
    progress_tx: &mpsc::Sender<transfers::Progress>,
    file_tx: &mpsc::Sender<FileIn>,
    listening: bool,
    browsing: &HashSet<String>,
    folders: &mut HashMap<String, Vec<(u32, String)>>,
    peer_gen: &mut u64,
    peer_generation: &mut HashMap<String, u64>,
) -> bool {
    match report {
        PeerReport::Direct {
            username,
            conn_type,
            stream: Ok(stream),
        } => {
            let from_parent = conn_type == ConnType::Distributed
                && search_state.pending_parent.as_deref() == Some(username.as_str());
            if conn_type == ConnType::File {
                if let Some(job) = transfers.upload_for_socket(&username) {
                    let limit = transfers.upload_limit();
                    let tx = progress_tx.clone();
                    tasks.handles.push(tokio::spawn(async move {
                        let progress = transfers::send_upload(stream, job, limit, &tx).await;
                        let _ = tx.send(progress).await;
                    }));
                } else {
                    book.release();
                }
                let _ = events
                    .send(SessionEvent::PeerReady {
                        username,
                        conn_type,
                        path: PeerPath::DirectOut,
                    })
                    .await;
                return true;
            }
            let generation = note_peer_generation(&username, conn_type, peer_gen, peer_generation);
            let mut write = watch_peer(
                stream,
                Vec::new(),
                &username,
                conn_type,
                from_parent,
                generation,
                tasks,
                frame_tx,
                distrib_tx,
            );
            if from_parent {
                search_state.parent = Some(username.clone());
                search_state.pending_parent = None;
                idle_writers.push(write);
                let _ = events
                    .send(SessionEvent::Network {
                        parent: search_state.parent.clone(),
                        children: u32::try_from(search_state.child_count).unwrap_or(u32::MAX),
                    })
                    .await;
                if send_message(link, &ClientMessage::HaveNoParent(false))
                    .await
                    .is_err()
                {
                    return false;
                }
            } else if conn_type == ConnType::Distributed {
                idle_writers.push(write);
            } else if write_download_asks(&mut write, transfers, &username).await {
                if browsing.contains(&username) {
                    let _ = write_browse(&mut write).await;
                }
                flush_folders(&mut write, &username, folders).await;
                peer_writers.insert(username.clone(), write);
            }
            let _ = events
                .send(SessionEvent::PeerReady {
                    username,
                    conn_type,
                    path: PeerPath::DirectOut,
                })
                .await;
        }
        PeerReport::Direct {
            username,
            conn_type,
            stream: Err(()),
        } => {
            if !listening && conn_type == ConnType::Peer {
                emit_transfers(
                    events,
                    transfers.fail_queued(&username, TransferState::ConnectionTimeout),
                )
                .await;
            }
            if !fallback_indirect(link, events, book, &username, conn_type, listening).await {
                return false;
            }
        }
        PeerReport::Incoming {
            stream,
            handshake:
                Ok((
                    PeerHandshake::PeerInit {
                        username,
                        conn_type,
                    },
                    leftover,
                )),
        } => {
            if !book.try_reserve() {
                recover_peer_slots(book, transfers, events).await;
                if !book.try_reserve() {
                    let _ = events
                        .send(SessionEvent::PeerFailed {
                            username,
                            message: "peer connection cap reached".to_owned(),
                        })
                        .await;
                    return true;
                }
            }
            if conn_type == ConnType::File {
                spawn_file_read(stream, leftover, username.clone(), tasks, file_tx);
                let _ = events
                    .send(SessionEvent::PeerReady {
                        username,
                        conn_type,
                        path: PeerPath::DirectIn,
                    })
                    .await;
                return true;
            }
            let generation = note_peer_generation(&username, conn_type, peer_gen, peer_generation);
            let mut write = watch_peer(
                stream, leftover, &username, conn_type, false, generation, tasks, frame_tx,
                distrib_tx,
            );
            if conn_type == ConnType::Distributed {
                search_state.children.push(write);
                search_state.child_count = search_state.child_count.saturating_add(1);
                let _ = events
                    .send(SessionEvent::Network {
                        parent: search_state.parent.clone(),
                        children: u32::try_from(search_state.child_count).unwrap_or(u32::MAX),
                    })
                    .await;
            } else if write_download_asks(&mut write, transfers, &username).await {
                if browsing.contains(&username) {
                    let _ = write_browse(&mut write).await;
                }
                flush_folders(&mut write, &username, folders).await;
                peer_writers.insert(username.clone(), write);
            }
            let _ = events
                .send(SessionEvent::PeerReady {
                    username,
                    conn_type,
                    path: PeerPath::DirectIn,
                })
                .await;
        }
        PeerReport::Incoming {
            stream,
            handshake: Ok((PeerHandshake::PierceFireWall { token }, leftover)),
        } => {
            if let Some((username, conn_type)) = book.take_indirect(token) {
                if conn_type == ConnType::File {
                    if let Some(job) = transfers.upload_for_socket(&username) {
                        let limit = transfers.upload_limit();
                        let tx = progress_tx.clone();
                        tasks.handles.push(tokio::spawn(async move {
                            let progress = transfers::send_upload(stream, job, limit, &tx).await;
                            let _ = tx.send(progress).await;
                        }));
                    } else {
                        held.push(stream);
                    }
                } else {
                    keep_peer(
                        stream,
                        leftover,
                        &username,
                        conn_type,
                        tasks,
                        frame_tx,
                        distrib_tx,
                        search_state,
                        transfers,
                        peer_writers,
                        browsing,
                        folders,
                        events,
                        peer_gen,
                        peer_generation,
                    )
                    .await;
                }
                let _ = events
                    .send(SessionEvent::PeerReady {
                        username,
                        conn_type,
                        path: PeerPath::Indirect { token },
                    })
                    .await;
            }
        }
        PeerReport::Incoming {
            handshake: Err(_), ..
        } => {}
        PeerReport::Pierce {
            username,
            conn_type,
            token,
            stream: Ok(stream),
        } => {
            if conn_type == ConnType::File {
                spawn_file_read(stream, Vec::new(), username.clone(), tasks, file_tx);
            } else {
                keep_peer(
                    stream,
                    Vec::new(),
                    &username,
                    conn_type,
                    tasks,
                    frame_tx,
                    distrib_tx,
                    search_state,
                    transfers,
                    peer_writers,
                    browsing,
                    folders,
                    events,
                    peer_gen,
                    peer_generation,
                )
                .await;
            }
            let _ = events
                .send(SessionEvent::PeerReady {
                    username,
                    conn_type,
                    path: PeerPath::Pierce { token },
                })
                .await;
        }
        PeerReport::Pierce {
            username,
            token,
            stream: Err(()),
            ..
        } => {
            book.release();
            if send_message(link, &ClientMessage::CantConnectToPeer { token, username })
                .await
                .is_err()
            {
                return false;
            }
        }
        PeerReport::Held(stream) => {
            held.push(stream);
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn keep_peer(
    stream: TcpStream,
    leftover: Vec<u8>,
    username: &str,
    conn_type: ConnType,
    tasks: &mut PeerTasks,
    frame_tx: &mpsc::Sender<search::PeerIn>,
    distrib_tx: &mpsc::Sender<search::DistribIn>,
    search_state: &mut search::SearchState,
    transfers: &mut transfers::Book,
    peer_writers: &mut HashMap<String, OwnedWriteHalf>,
    browsing: &HashSet<String>,
    folders: &mut HashMap<String, Vec<(u32, String)>>,
    events: &mpsc::Sender<SessionEvent>,
    peer_gen: &mut u64,
    peer_generation: &mut HashMap<String, u64>,
) {
    let generation = note_peer_generation(username, conn_type, peer_gen, peer_generation);
    let mut write = watch_peer(
        stream, leftover, username, conn_type, false, generation, tasks, frame_tx, distrib_tx,
    );
    if conn_type == ConnType::Distributed {
        search_state.children.push(write);
        search_state.child_count = search_state.child_count.saturating_add(1);
        let _ = events
            .send(SessionEvent::Network {
                parent: search_state.parent.clone(),
                children: u32::try_from(search_state.child_count).unwrap_or(u32::MAX),
            })
            .await;
        return;
    }
    if write_download_asks(&mut write, transfers, username).await {
        if browsing.contains(username) {
            let _ = write_browse(&mut write).await;
        }
        flush_folders(&mut write, username, folders).await;
        peer_writers.insert(username.to_owned(), write);
    }
}

#[allow(clippy::too_many_arguments)]
fn watch_peer(
    stream: TcpStream,
    leftover: Vec<u8>,
    username: &str,
    conn_type: ConnType,
    from_parent: bool,
    generation: u64,
    tasks: &mut PeerTasks,
    frame_tx: &mpsc::Sender<search::PeerIn>,
    distrib_tx: &mpsc::Sender<search::DistribIn>,
) -> OwnedWriteHalf {
    let (read, write) = stream.into_split();
    if conn_type == ConnType::Distributed {
        let distrib_tx = distrib_tx.clone();
        tasks.handles.push(tokio::spawn(async move {
            search::read_distrib(read, leftover, from_parent, distrib_tx).await;
        }));
    } else {
        let frame_tx = frame_tx.clone();
        let username = username.to_owned();
        tasks.handles.push(tokio::spawn(async move {
            search::read_peer_frames(read, leftover, username, generation, frame_tx).await;
        }));
    }
    write
}

fn note_peer_generation(
    username: &str,
    conn_type: ConnType,
    peer_gen: &mut u64,
    peer_generation: &mut HashMap<String, u64>,
) -> u64 {
    if conn_type != ConnType::Peer {
        return 0;
    }
    *peer_gen = peer_gen.wrapping_add(1).max(1);
    peer_generation.insert(username.to_owned(), *peer_gen);
    *peer_gen
}

async fn write_download_asks(
    write: &mut OwnedWriteHalf,
    transfers: &mut transfers::Book,
    user: &str,
) -> bool {
    let mut frames = transfers.take_queue_uploads(user);
    frames.extend(transfers.place_requests(user));
    for frame in frames {
        if write.write_all(&frame).await.is_err() {
            transfers.requeue_asks(user);
            return false;
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn forget_peer(
    username: &str,
    generation: u64,
    peer_generation: &mut HashMap<String, u64>,
    peer_writers: &mut HashMap<String, OwnedWriteHalf>,
    book: &mut PeerBook,
    transfers: &mut transfers::Book,
    link: &mut ServerLink,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    book.release();
    if peer_generation.get(username).copied() != Some(generation) {
        return true;
    }
    peer_generation.remove(username);
    peer_writers.remove(username);
    transfers.requeue_asks(username);
    if !transfers.has_queued(username) || book.connecting(username, ConnType::Peer) {
        return true;
    }
    ask_peer(link, book, events, transfers, username, ConnType::Peer).await
}

async fn flush_folders(
    writer: &mut OwnedWriteHalf,
    username: &str,
    folders: &mut HashMap<String, Vec<(u32, String)>>,
) {
    let Some(pending) = folders.remove(username) else {
        return;
    };
    for (token, directory) in pending {
        if write_folder(writer, token, &directory).await.is_err() {
            return;
        }
    }
}

async fn write_folder(writer: &mut OwnedWriteHalf, token: u32, directory: &str) -> io::Result<()> {
    let frame = crate::protocol::peer::encode_folder_contents_request(
        &crate::protocol::peer::FolderContentsRequest {
            token,
            directory: directory.to_owned(),
        },
    )
    .map_err(io::Error::other)?;
    writer.write_all(&frame).await
}

async fn write_browse(writer: &mut OwnedWriteHalf) -> io::Result<()> {
    let list =
        crate::protocol::peer::encode_shared_file_list_request().map_err(io::Error::other)?;
    let info = crate::protocol::peer::encode_user_info_request().map_err(io::Error::other)?;
    writer.write_all(&list).await?;
    writer.write_all(&info).await?;
    Ok(())
}

async fn emit_transfers(events: &mpsc::Sender<SessionEvent>, outs: Vec<transfers::Out>) {
    for out in outs {
        match out {
            transfers::Out::Event(transfer) => {
                let _ = events.send(SessionEvent::Transfer(transfer)).await;
            }
            transfers::Out::AlbumFailed { user, album } => {
                let _ = events.send(SessionEvent::AlbumFailed { user, album }).await;
            }
            transfers::Out::FileUnshared {
                user,
                file,
                path,
                folder,
            } => {
                let _ = events
                    .send(SessionEvent::FileUnshared {
                        user,
                        file,
                        path,
                        folder,
                    })
                    .await;
            }
            transfers::Out::Write { .. } | transfers::Out::ConnectFile { .. } => {}
        }
    }
}

async fn retry_connection_downloads(
    link: &mut ServerLink,
    book: &mut PeerBook,
    peer_writers: &mut HashMap<String, OwnedWriteHalf>,
    transfers: &mut transfers::Book,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    let (outs, users) = transfers.retry_connection_failures();
    emit_transfers(events, outs).await;
    for user in users {
        if let Some(writer) = peer_writers.get_mut(&user) {
            if !write_download_asks(writer, transfers, &user).await {
                peer_writers.remove(&user);
            }
        } else if !book.connecting(&user, ConnType::Peer)
            && !ask_peer(link, book, events, transfers, &user, ConnType::Peer).await
        {
            return false;
        }
    }
    true
}

async fn ask_peer(
    link: &mut ServerLink,
    book: &mut PeerBook,
    events: &mpsc::Sender<SessionEvent>,
    transfers: &mut transfers::Book,
    username: &str,
    conn_type: ConnType,
) -> bool {
    if !book.try_reserve() {
        recover_peer_slots(book, transfers, events).await;
        if !book.try_reserve() {
            if conn_type == ConnType::Peer {
                emit_transfers(
                    events,
                    transfers.fail_queued(username, TransferState::ConnectionClosed),
                )
                .await;
            }
            let _ = events
                .send(SessionEvent::PeerFailed {
                    username: username.to_owned(),
                    message: "peer connection cap reached".to_owned(),
                })
                .await;
            return true;
        }
    }
    if !link.live {
        return true;
    }
    book.push_wait(username.to_owned(), conn_type);
    if let Err(event) =
        send_message(link, &ClientMessage::GetPeerAddress(username.to_owned())).await
    {
        book.take_wait(username);
        book.release();
        let _ = events.send(event).await;
        return false;
    }
    true
}

async fn apply_outs(
    outs: Vec<transfers::Out>,
    link: &mut ServerLink,
    book: &mut PeerBook,
    peer_writers: &mut HashMap<String, OwnedWriteHalf>,
    transfers: &mut transfers::Book,
    events: &mpsc::Sender<SessionEvent>,
) -> bool {
    for out in outs {
        match out {
            transfers::Out::Write { user, frame } => {
                if let Some(writer) = peer_writers.get_mut(&user) {
                    let _ = writer.write_all(&frame).await;
                }
            }
            transfers::Out::ConnectFile { user } => {
                if !ask_peer(link, book, events, transfers, &user, ConnType::File).await {
                    return false;
                }
            }
            transfers::Out::Event(transfer) => {
                let _ = events.send(SessionEvent::Transfer(transfer)).await;
            }
            transfers::Out::AlbumFailed { user, album } => {
                let _ = events.send(SessionEvent::AlbumFailed { user, album }).await;
            }
            transfers::Out::FileUnshared {
                user,
                file,
                path,
                folder,
            } => {
                let _ = events
                    .send(SessionEvent::FileUnshared {
                        user,
                        file,
                        path,
                        folder,
                    })
                    .await;
            }
        }
    }
    true
}

async fn fallback_indirect(
    link: &mut ServerLink,
    events: &mpsc::Sender<SessionEvent>,
    book: &mut PeerBook,
    username: &str,
    conn_type: ConnType,
    listening: bool,
) -> bool {
    if !listening || !link.live {
        book.release();
        if !listening {
            let _ = events
                .send(SessionEvent::PeerFailed {
                    username: username.to_owned(),
                    message: "listen socket is not open".to_owned(),
                })
                .await;
        }
        return true;
    }
    let token = book.begin_indirect(username.to_owned(), conn_type);
    if let Err(event) = send_message(
        link,
        &ClientMessage::ConnectToPeer {
            token,
            username: username.to_owned(),
            conn_type,
        },
    )
    .await
    {
        book.take_indirect(token);
        book.release();
        let _ = events.send(event).await;
        return false;
    }
    true
}

/// Drops address lookups and indirect connects that have already timed out, and frees their slots.
async fn recover_peer_slots(
    book: &mut PeerBook,
    transfers: &mut transfers::Book,
    events: &mpsc::Sender<SessionEvent>,
) {
    let stale = book
        .expire_waiting(CONNECT_TIMEOUT)
        .into_iter()
        .chain(book.expire_indirect(CONNECT_TIMEOUT))
        .collect::<Vec<_>>();
    for (user, conn_type) in stale {
        if conn_type == ConnType::Peer {
            emit_transfers(
                events,
                transfers.fail_queued(&user, TransferState::ConnectionTimeout),
            )
            .await;
        }
        let _ = events
            .send(SessionEvent::PeerFailed {
                username: user,
                message: "connection timeout".to_owned(),
            })
            .await;
    }
}

fn peer_addr(ip: Ipv4Addr, port: u32) -> Option<SocketAddr> {
    let port = u16::try_from(port).ok().filter(|port| *port != 0)?;
    Some(SocketAddr::from((ip, port)))
}

/// `Ok` is the peer-related frames in this read. `Err` stops the server loop.
async fn dispatch(
    decoder: &mut FrameDecoder,
    events: &mpsc::Sender<SessionEvent>,
    rooms: &mut chat::RoomChat,
) -> Result<Vec<ServerAction>, ()> {
    let mut actions = Vec::new();
    loop {
        let frame = match decoder.pop() {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(actions),
            Err(err) => {
                let _ = events.send(protocol_event(err)).await;
                return Err(());
            }
        };
        if frame.code == server::RELOGGED {
            let _ = events.send(SessionEvent::Kicked).await;
            return Err(());
        }
        match rooms.on_frame(frame.code, &frame.payload).await {
            Ok(Some(effects)) => {
                for effect in effects {
                    match effect {
                        chat::Effect::Send(frame) => actions.push(ServerAction::SendFrame(frame)),
                        chat::Effect::Event(event) => {
                            let _ = events.send(event).await;
                        }
                    }
                }
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                let _ = events.send(protocol_event(err)).await;
                return Err(());
            }
        }
        if frame.code == server::WATCH_USER {
            match decode_watch_user(&frame.payload) {
                Ok(reply) => {
                    let _ = events
                        .send(SessionEvent::Watched {
                            user: reply.user,
                            status: reply.status,
                            country: reply.country,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::GET_USER_STATS {
            match decode_user_stats(&frame.payload) {
                Ok(stats) => {
                    let _ = events
                        .send(SessionEvent::UserStats {
                            user: stats.user,
                            files: stats.files,
                            dirs: stats.dirs,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::USER_INTERESTS {
            match decode_user_interests(&frame.payload) {
                Ok(interests) => {
                    let _ = events
                        .send(SessionEvent::Interests {
                            user: interests.user,
                            likes: interests.likes,
                            hates: interests.hates,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::RECOMMENDATIONS
            || frame.code == server::GLOBAL_RECOMMENDATIONS
        {
            match decode_recommendations(&frame.payload) {
                Ok(items) => {
                    let _ = events.send(SessionEvent::Recommendations(items)).await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::SIMILAR_USERS {
            match decode_similar_users(&frame.payload) {
                Ok(users) => {
                    let _ = events.send(SessionEvent::SimilarUsers(users)).await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::GET_USER_STATUS {
            match decode_user_status(&frame.payload) {
                Ok(status) => {
                    actions.push(ServerAction::UserFlag {
                        user: status.user.clone(),
                        privileged: status.privileged,
                    });
                    if status.status == UserStatus::Offline as u32 {
                        actions.push(ServerAction::PeerOffline(status.user.clone()));
                    }
                    let _ = events
                        .send(SessionEvent::UserStatus {
                            user: status.user,
                            status: status.status,
                            privileged: status.privileged,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::GET_PEER_ADDRESS {
            match decode_peer_address(&frame.payload) {
                Ok(address) => actions.push(ServerAction::PeerAddress(address)),
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::CANT_CONNECT_TO_PEER {
            match decode_cant_connect(&frame.payload) {
                Ok(token) => actions.push(ServerAction::CantConnect { token }),
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::CONNECT_TO_PEER {
            match decode_incoming_connect(&frame.payload) {
                Ok(request) => actions.push(ServerAction::ConnectToPeer(request)),
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::FILE_SEARCH {
            match decode_incoming_file_search(&frame.payload) {
                Ok(search) => actions.push(ServerAction::IncomingSearch {
                    username: search.username,
                    token: search.token,
                    query: search.query,
                }),
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        } else if frame.code == server::WISHLIST_INTERVAL {
            if decode_wishlist_interval(&frame.payload).is_ok() {
                actions.push(ServerAction::WishlistInterval);
            }
        } else if frame.code == server::POSSIBLE_PARENTS {
            match decode_possible_parents(&frame.payload) {
                Ok(parents) => actions.push(ServerAction::PossibleParents(parents)),
                Err(err) => {
                    let _ = events.send(protocol_event(err)).await;
                    return Err(());
                }
            }
        }
    }
}

async fn deadline_io<T>(
    deadline: Instant,
    work: impl std::future::Future<Output = io::Result<T>>,
) -> Result<T, SessionEvent> {
    let Some(left) = remaining(deadline) else {
        return Err(SessionEvent::TimedOut);
    };
    match tokio::time::timeout(left, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(SessionEvent::Disconnected {
            message: err.to_string(),
        }),
        Err(_) => Err(SessionEvent::TimedOut),
    }
}

async fn read_frame(link: &mut ServerLink, deadline: Instant) -> Result<ServerFrame, SessionEvent> {
    let mut buf = [0u8; 8192];
    loop {
        if let Some(frame) = next_frame(&mut link.decoder)? {
            return Ok(frame);
        }
        let Some(left) = remaining(deadline) else {
            return Err(SessionEvent::TimedOut);
        };
        let read = tokio::time::timeout(left, link.stream.read(&mut buf)).await;
        let count = match read {
            Ok(Ok(0)) => {
                return Err(SessionEvent::Disconnected {
                    message: "server closed the connection".to_owned(),
                });
            }
            Ok(Ok(count)) => count,
            Ok(Err(err)) => {
                return Err(SessionEvent::Disconnected {
                    message: err.to_string(),
                });
            }
            Err(_) => return Err(SessionEvent::TimedOut),
        };
        link.decoder.push(&buf[..count]);
    }
}

#[allow(clippy::result_large_err)]
fn next_frame(decoder: &mut FrameDecoder) -> Result<Option<ServerFrame>, SessionEvent> {
    match decoder.pop() {
        Ok(frame) => Ok(frame),
        Err(err) => Err(protocol_event(err)),
    }
}

fn protocol_event(err: ProtocolError) -> SessionEvent {
    SessionEvent::Disconnected {
        message: err.to_string(),
    }
}

fn remaining(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use tokio::net::{TcpListener, TcpStream};

    use super::fixture::Fixture;
    use super::*;
    use crate::protocol::{FrameDecoder, UserStatus, encode_frame};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn alice(addr: std::net::SocketAddr) -> Config {
        let mut config = Config {
            username: "alice".to_owned(),
            server_host: addr.ip().to_string(),
            server_port: addr.port(),
            ..Config::default()
        };
        config.set_password("secret");
        config
    }

    fn success_payload() -> Vec<u8> {
        hex(
            "010500000068656c6c6f0100007f20000000633465333133313332323263663035666364643166633036386166353537306501",
        )
    }

    fn failure_payload() -> Vec<u8> {
        hex("000b000000494e56414c494450415353")
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }

    async fn free_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        listener.local_addr().unwrap().port()
    }

    async fn read_exact(sock: &mut TcpStream, len: usize) -> Vec<u8> {
        let mut got = vec![0u8; len];
        sock.read_exact(&mut got).await.unwrap();
        got
    }

    #[test]
    fn a_specific_bind_wins_and_an_open_bind_uses_the_server_socket() {
        let vpn = IpAddr::V4(Ipv4Addr::new(10, 8, 0, 5));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(chosen_interface(Some(vpn), Some(lan)), "10.8.0.5");
        assert_eq!(
            chosen_interface(Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), Some(lan)),
            "192.168.1.20"
        );
        assert_eq!(
            chosen_interface(Some(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)), None),
            ""
        );
    }

    fn logged_in() -> SessionEvent {
        SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        }
    }

    #[tokio::test]
    async fn login_against_fixture() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let expected = ClientMessage::Login(LoginRequest::new("alice", "secret"))
            .frame()
            .unwrap();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait = ClientMessage::SetWaitPort(u32::from(port)).frame().unwrap();
        let status = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            assert_eq!(read_exact(&mut sock, expected.len()).await, expected);
            sock.write_all(&reply).await.unwrap();
            assert_eq!(read_exact(&mut sock, wait.len()).await, wait);
            assert_eq!(read_exact(&mut sock, status.len()).await, status);
            sock
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, logged_in());
        let listening = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            listening,
            SessionEvent::Listening {
                port,
                address: "127.0.0.1".to_owned(),
            }
        );
        let mut sock = server.await.unwrap();
        drop(session);
        expect_only_share_counts_then_close(&mut sock).await;
    }

    #[tokio::test]
    async fn listen_port_is_registered() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let golden_wait = ClientMessage::SetWaitPort(2234).frame().unwrap();
        let wait = ClientMessage::SetWaitPort(u32::from(port)).frame().unwrap();
        let status = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap();
        assert_eq!(golden_wait, hex("0800000002000000ba080000"));
        assert_eq!(status, hex("080000001c00000002000000"));
        let wait_len = wait.len();
        let status_len = status.len();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut login = [0u8; 256];
            let _ = sock.read(&mut login).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut sock_wait = vec![0u8; wait_len];
            sock.read_exact(&mut sock_wait).await.unwrap();
            let mut sock_status = vec![0u8; status_len];
            sock.read_exact(&mut sock_status).await.unwrap();
            (sock_wait, sock_status)
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, logged_in());
        assert_eq!(
            second,
            SessionEvent::Listening {
                port,
                address: "127.0.0.1".to_owned(),
            }
        );
        let (got_wait, got_status) = server.await.unwrap();
        assert_eq!(got_wait, wait);
        assert_eq!(got_status, status);
        drop(session);
    }

    #[tokio::test]
    async fn bind_failure_does_not_send_wait_port() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let held = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();
        let mut config = alice(addr);
        config.listen_port = port;
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut login = [0u8; 256];
            let _ = sock.read(&mut login).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut buf = [0u8; 32];
            let read = tokio::time::timeout(Duration::from_millis(400), sock.read(&mut buf)).await;
            read.map(|inner| inner.map(|count| buf[..count].to_vec()))
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, logged_in());
        match second {
            SessionEvent::ListenFailed { message } => {
                assert!(message.contains(&port.to_string()), "{message}");
                assert!(!message.contains("secret"));
            }
            other => panic!("expected ListenFailed, got {other:?}"),
        }
        match server.await.unwrap() {
            Err(_) => {}
            Ok(Ok(bytes)) => {
                assert!(is_share_count(&bytes), "SetWaitPort was written: {bytes:?}");
            }
            Ok(Err(err)) => panic!("{err}"),
        }
        drop(held);
        drop(session);
    }

    #[tokio::test]
    async fn relogged_stops_the_session() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let kicked = encode_frame(server::RELOGGED, &[]).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut login = [0u8; 256];
            let _ = sock.read(&mut login).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            sock.write_all(&kicked).await.unwrap();
            read_until_close(&mut sock).await
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let mut saw_kicked = false;
        for _ in 0..4 {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if event == SessionEvent::Kicked {
                saw_kicked = true;
                break;
            }
        }
        assert!(saw_kicked);
        let pending = server.await.unwrap();
        assert!(
            strip_share_counts(&pending).is_empty(),
            "bytes besides the share count: {pending:?}"
        );
        drop(session);
    }

    #[tokio::test]
    async fn user_status_from_the_server_is_decoded() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let notice = encode_frame(
            server::GET_USER_STATUS,
            &hex("05000000616c6963650200000001"),
        )
        .unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut login = [0u8; 256];
            let _ = sock.read(&mut login).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            sock.write_all(&notice).await.unwrap();
            sock
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let mut found = None;
        for _ in 0..4 {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::UserStatus {
                user,
                status,
                privileged,
            } = event
            {
                found = Some((user, status, privileged));
                break;
            }
        }
        assert_eq!(found, Some(("alice".to_owned(), 2, true)));
        drop(session);
        let mut sock = server.await.unwrap();
        expect_only_share_counts_then_close(&mut sock).await;
    }

    #[tokio::test]
    async fn ping_follows_an_idle_interval() {
        assert_eq!(PING_INTERVAL, Duration::from_secs(60));
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let ping = ClientMessage::Ping.frame().unwrap();
        assert_eq!(ping, hex("0400000020000000"));
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut login = [0u8; 256];
            let _ = sock.read(&mut login).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            let mut frames = FrameDecoder::new();
            loop {
                let frame = read_any_frame(&mut sock, &mut frames).await;
                if frame.code != server::SHARED_FOLDERS_FILES {
                    break frame;
                }
            }
        });

        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::start(config, tx, CONNECT_TIMEOUT, Duration::from_millis(80));
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.code, server::SERVER_PING);
        assert!(got.payload.is_empty());
        drop(session);
    }

    #[tokio::test]
    async fn login_failure_is_invalid_pass() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let reply = encode_frame(server::LOGIN, &failure_payload()).unwrap();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 256];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
        });

        let (tx, mut rx) = mpsc::channel(4);
        let session = Session::spawn(alice(addr), tx);
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            SessionEvent::LoginFailed {
                reason: "INVALIDPASS".to_owned()
            }
        );
        drop(session);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn login_times_out_when_the_fixture_sends_nothing() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let (hold, held) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let sock = fixture.accept().await.unwrap();
            let _ = held.await;
            drop(sock);
        });

        let (tx, mut rx) = mpsc::channel(4);
        let session = Session::spawn_timeout(alice(addr), tx, Duration::from_millis(200));
        let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, SessionEvent::TimedOut);
        drop(hold);
        drop(session);
        server.await.unwrap();
    }

    use crate::protocol::{
        ConnType, PeerHandshake, PeerInitDecoder, encode_peer_init, encode_pierce_firewall,
    };

    async fn online(port: u16, cap: usize) -> (Session, mpsc::Receiver<SessionEvent>, TcpStream) {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let mut config = alice(addr);
        config.listen_port = port;
        let expected = ClientMessage::Login(LoginRequest::new("alice", "secret"))
            .frame()
            .unwrap();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait = ClientMessage::SetWaitPort(u32::from(port)).frame().unwrap();
        let status = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            assert_eq!(read_exact(&mut sock, expected.len()).await, expected);
            sock.write_all(&reply).await.unwrap();
            assert_eq!(read_exact(&mut sock, wait.len()).await, wait);
            assert_eq!(read_exact(&mut sock, status.len()).await, status);
            let _ = ready_tx.send(sock);
        });
        let (tx, mut rx) = mpsc::channel(32);
        let session = Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, cap);
        let mut listening = false;
        for _ in 0..4 {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Listening { port: got, .. } = event {
                assert_eq!(got, port);
                listening = true;
                break;
            }
        }
        assert!(listening);
        let sock = ready_rx.await.unwrap();
        (session, rx, sock)
    }

    async fn read_server_frame(sock: &mut TcpStream, decoder: &mut FrameDecoder) -> ServerFrame {
        loop {
            let frame = read_any_frame(sock, decoder).await;
            if frame.code != server::SHARED_FOLDERS_FILES {
                return frame;
            }
        }
    }

    async fn read_any_frame(sock: &mut TcpStream, decoder: &mut FrameDecoder) -> ServerFrame {
        let mut buf = [0u8; 1024];
        loop {
            if let Some(frame) = decoder.pop().unwrap() {
                return frame;
            }
            let count = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert_ne!(count, 0);
            decoder.push(&buf[..count]);
        }
    }

    fn is_share_count(bytes: &[u8]) -> bool {
        bytes.len() >= 8
            && u32::from_le_bytes(bytes[0..4].try_into().unwrap()) == 12
            && u32::from_le_bytes(bytes[4..8].try_into().unwrap()) == server::SHARED_FOLDERS_FILES
    }

    fn strip_share_counts(buf: &[u8]) -> &[u8] {
        let mut rest = buf;
        while rest.len() >= 16 && is_share_count(rest) {
            rest = &rest[16..];
        }
        if !rest.is_empty() && is_share_count(rest) {
            return &[];
        }
        rest
    }

    async fn read_until_close(sock: &mut TcpStream) -> Vec<u8> {
        let mut pending = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let count = tokio::time::timeout(Duration::from_secs(2), sock.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            if count == 0 {
                return pending;
            }
            pending.extend_from_slice(&buf[..count]);
        }
    }

    async fn expect_only_share_counts_then_close(sock: &mut TcpStream) {
        let pending = read_until_close(sock).await;
        assert!(
            strip_share_counts(&pending).is_empty(),
            "bytes besides the share count: {pending:?}"
        );
    }

    fn address_payload(port: u16) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"bob");
        payload.extend_from_slice(&[0x01, 0x00, 0x00, 0x7f]);
        payload.extend_from_slice(&u32::from(port).to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload
    }

    async fn read_handshake(stream: &mut TcpStream) -> (PeerHandshake, Vec<u8>) {
        let mut decoder = PeerInitDecoder::new();
        let mut buf = [0u8; 256];
        loop {
            if let Some(message) = decoder.pop().unwrap() {
                return (message, decoder.into_remainder());
            }
            let count = stream.read(&mut buf).await.unwrap();
            decoder.push(&buf[..count]);
        }
    }

    #[tokio::test]
    async fn peer_init_both_directions() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        let peer = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = peer.local_addr().unwrap().port();
        session.connect_peer("bob", ConnType::Peer).await;
        let mut frames = FrameDecoder::new();
        let request = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        assert_eq!(
            request.payload,
            ClientMessage::GetPeerAddress("bob".to_owned())
                .payload()
                .unwrap()
        );
        let reply = encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap();
        server.write_all(&reply).await.unwrap();

        let (mut inbound, _) = tokio::time::timeout(Duration::from_secs(2), peer.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read_handshake(&mut inbound).await.0,
            PeerHandshake::PeerInit {
                username: "alice".to_owned(),
                conn_type: ConnType::Peer,
            }
        );

        let mut back = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        back.write_all(&encode_peer_init("bob", ConnType::Peer).unwrap())
            .await
            .unwrap();

        let mut paths = Vec::new();
        for _ in 0..2 {
            let event = next_event(&mut events).await;
            let SessionEvent::PeerReady {
                username,
                conn_type,
                path,
            } = event
            else {
                panic!("unexpected {event:?}");
            };
            assert_eq!(username, "bob");
            assert_eq!(conn_type, ConnType::Peer);
            paths.push(path);
        }
        assert!(paths.contains(&PeerPath::DirectOut));
        assert!(paths.contains(&PeerPath::DirectIn));
        drop(session);
    }

    #[tokio::test]
    async fn refused_direct_falls_back_to_indirect_token() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        session.connect_peer("bob", ConnType::File).await;
        let mut frames = FrameDecoder::new();
        let request = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let reply = encode_frame(server::GET_PEER_ADDRESS, &address_payload(1)).unwrap();
        server.write_all(&reply).await.unwrap();

        let indirect = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(indirect.code, server::CONNECT_TO_PEER);
        let token = u32::from_le_bytes(indirect.payload[0..4].try_into().unwrap());
        let expected = ClientMessage::ConnectToPeer {
            token,
            username: "bob".to_owned(),
            conn_type: ConnType::File,
        }
        .payload()
        .unwrap();
        assert_eq!(indirect.payload, expected);

        let mut back = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        back.write_all(&encode_pierce_firewall(token).unwrap())
            .await
            .unwrap();
        let event = next_event(&mut events).await;
        assert_eq!(
            event,
            SessionEvent::PeerReady {
                username: "bob".to_owned(),
                conn_type: ConnType::File,
                path: PeerPath::Indirect { token },
            }
        );
        drop(session);
    }

    #[tokio::test]
    async fn a_refused_pierce_tells_the_server_without_a_notice() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"bob");
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(b"P");
        payload.extend_from_slice(&[0x01, 0x00, 0x00, 0x7f]);
        payload.extend_from_slice(&u32::from(closed_port).to_le_bytes());
        payload.extend_from_slice(&9u32.to_le_bytes());
        payload.push(0);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        server
            .write_all(&encode_frame(server::CONNECT_TO_PEER, &payload).unwrap())
            .await
            .unwrap();
        let reply = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(reply.code, server::CANT_CONNECT_TO_PEER);
        assert_eq!(
            reply.payload,
            ClientMessage::CantConnectToPeer {
                token: 9,
                username: "bob".to_owned(),
            }
            .payload()
            .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(
                    event,
                    SessionEvent::PeerFailed { ref message, .. } if message == "pierce connection failed"
                ),
                "{event:?}"
            );
        }
        drop(session);
    }

    #[tokio::test]
    async fn connect_to_peer_is_answered_with_pierce() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        let peer = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = peer.local_addr().unwrap().port();
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"bob");
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(b"P");
        payload.extend_from_slice(&[0x01, 0x00, 0x00, 0x7f]);
        payload.extend_from_slice(&u32::from(peer_port).to_le_bytes());
        payload.extend_from_slice(&9u32.to_le_bytes());
        payload.push(0);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        let frame = encode_frame(server::CONNECT_TO_PEER, &payload).unwrap();
        server.write_all(&frame).await.unwrap();

        let (mut inbound, _) = tokio::time::timeout(Duration::from_secs(2), peer.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read_handshake(&mut inbound).await.0,
            PeerHandshake::PierceFireWall { token: 9 }
        );
        let event = next_event(&mut events).await;
        assert_eq!(
            event,
            SessionEvent::PeerReady {
                username: "bob".to_owned(),
                conn_type: ConnType::Peer,
                path: PeerPath::Pierce { token: 9 },
            }
        );
        drop(session);
    }

    #[tokio::test]
    async fn pierce_delivers_a_search_result() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        session
            .search(SearchRequest {
                mode: crate::model::SearchMode::Global,
                query: "green day".to_owned(),
                room: String::new(),
                user: String::new(),
            })
            .await;
        let mut frames = FrameDecoder::new();
        let sent = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(sent.code, server::FILE_SEARCH);
        let token = u32::from_le_bytes(sent.payload[0..4].try_into().unwrap());
        let mut query = &sent.payload[4..];
        let len = usize::try_from(u32::from_le_bytes(query[..4].try_into().unwrap())).unwrap();
        query = &query[4..];
        assert_eq!(&query[..len], b"green day");

        let peer = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = peer.local_addr().unwrap().port();
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"bob");
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(b"P");
        payload.extend_from_slice(&[0x01, 0x00, 0x00, 0x7f]);
        payload.extend_from_slice(&u32::from(peer_port).to_le_bytes());
        payload.extend_from_slice(&9u32.to_le_bytes());
        payload.push(0);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        server
            .write_all(&encode_frame(server::CONNECT_TO_PEER, &payload).unwrap())
            .await
            .unwrap();
        let (mut inbound, _) = tokio::time::timeout(Duration::from_secs(2), peer.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read_handshake(&mut inbound).await.0,
            PeerHandshake::PierceFireWall { token: 9 }
        );
        let response = FileSearchResponse {
            username: "bob".to_owned(),
            token,
            files: vec![crate::protocol::SearchResultFile {
                path: "music\\basket-case.mp3".to_owned(),
                size: 9,
                attributes: Vec::new(),
            }],
            free_slot: true,
            upload_speed: 0,
            queue: 0,
            private_files: Vec::new(),
        };
        inbound
            .write_all(
                &encode_frame(
                    crate::protocol::peer::FILE_SEARCH_RESPONSE,
                    &response.encode().unwrap(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let hit = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::SearchResult(_, hit) = event {
                break hit;
            }
        };
        assert_eq!(hit.user, "bob");
        assert_eq!(hit.path, "music\\basket-case.mp3");
        drop(session);
    }

    #[tokio::test]
    async fn folder_contents_returns_both_album_files() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        session
            .folder("bob".to_owned(), "music\\Dookie".to_owned())
            .await;
        let mut frames = FrameDecoder::new();
        let request = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut inbound, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut inbound).await;
        let mut peer_frames = FrameDecoder::new();
        peer_frames.push(&remainder);
        let asked = read_server_frame(&mut inbound, &mut peer_frames).await;
        assert_eq!(asked.code, crate::protocol::peer::FOLDER_CONTENTS_REQUEST);
        let decoded =
            crate::protocol::peer::decode_folder_contents_request(&asked.payload).unwrap();
        assert_eq!(decoded.directory, "music\\Dookie");
        let response = crate::protocol::peer::FolderContentsResponse {
            token: decoded.token,
            directory: "music\\Dookie".to_owned(),
            folders: vec![crate::protocol::peer::SharedFolder {
                directory: "music\\Dookie".to_owned(),
                files: vec![
                    crate::protocol::peer::SharedFile {
                        name: "01.flac".to_owned(),
                        size: 4,
                        attributes: Vec::new(),
                    },
                    crate::protocol::peer::SharedFile {
                        name: "02.flac".to_owned(),
                        size: 8,
                        attributes: Vec::new(),
                    },
                ],
            }],
        };
        inbound
            .write_all(&crate::protocol::peer::encode_folder_contents_response(&response).unwrap())
            .await
            .unwrap();
        let files = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Folder {
                files, directory, ..
            } = event
            {
                assert_eq!(directory, "music\\Dookie");
                break files;
            }
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "music\\Dookie\\01.flac");
        assert_eq!(files[1].path, "music\\Dookie\\02.flac");
        assert_eq!(files[1].size, 8);
        drop(session);
    }

    #[tokio::test]
    async fn a_finished_file_frees_its_connection_slot() {
        let root = std::env::temp_dir().join(format!("soul-sever-slot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut events) = mpsc::channel(32);
        let session = Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, 2);
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let mut server = ready_rx.await.unwrap();
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 4,
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 7, 4, 0).await;
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &[1, 2, 3, 4]).await;
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, 4);
        drop(file);
        session.connect_peer("carol", ConnType::Peer).await;
        let next = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(next.code, server::GET_PEER_ADDRESS);
        assert!(next.payload.windows(5).any(|bytes| bytes == b"carol"));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_closed_server_does_not_stop_the_download() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-server-down-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut events) = mpsc::channel(32);
        let session = Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, 2);
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let mut server = ready_rx.await.unwrap();
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 4,
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 7, 4, 0).await;
        drop(server);
        let mut closed = false;
        for _ in 0..8 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Disconnected { message } = event {
                assert!(message.contains("reconnecting"), "{message}");
                closed = true;
                break;
            }
        }
        assert!(closed);
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &[1, 2, 3, 4]).await;
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, 4);
        assert_eq!(
            std::fs::read(download.join("tone.bin")).unwrap(),
            [1, 2, 3, 4]
        );
        drop(file);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn broken_pipe_is_a_reconnecting_disconnect() {
        let event = lost_server(&io::Error::from_raw_os_error(32));
        assert_eq!(
            event,
            SessionEvent::Disconnected {
                message: "Broken pipe (os error 32). reconnecting".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn a_broken_pipe_does_not_stop_the_download() {
        let root =
            std::env::temp_dir().join(format!("soul-sever-broken-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut events) = mpsc::channel(32);
        let session = Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, 8);
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let mut server = ready_rx.await.unwrap();
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 4,
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 7, 4, 0).await;
        drop(server);
        let mut closed = false;
        for _ in 0..8 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Disconnected { message } = event {
                assert!(message.contains("reconnecting"), "{message}");
                closed = true;
                break;
            }
        }
        assert!(closed);
        session.connect_peer("carol", ConnType::Peer).await;
        session.connect_peer("dave", ConnType::Peer).await;
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(SessionEvent::Disconnected { message })) => {
                    panic!("wrote to a closed server socket: {message}")
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("session ended"),
                Err(_) => break,
            }
        }
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &[1, 2, 3, 4]).await;
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, 4);
        assert_eq!(
            std::fs::read(download.join("tone.bin")).unwrap(),
            [1, 2, 3, 4]
        );
        drop(file);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn peer_cap_rejects_without_asking_the_server() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, 0).await;
        session.connect_peer("bob", ConnType::Peer).await;
        let event = next_event(&mut events).await;
        assert_eq!(
            event,
            SessionEvent::PeerFailed {
                username: "bob".to_owned(),
                message: "peer connection cap reached".to_owned(),
            }
        );
        let mut buf = [0u8; 8];
        let read = tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf)).await;
        match read {
            Err(_) => {}
            Ok(Ok(count)) if is_share_count(&buf[..count]) => {
                let mut rest = [0u8; 8];
                server.read_exact(&mut rest).await.unwrap();
                let again =
                    tokio::time::timeout(Duration::from_millis(200), server.read(&mut buf)).await;
                assert!(again.is_err(), "GetPeerAddress was written");
            }
            other => panic!("GetPeerAddress was written: {other:?}"),
        }
        drop(session);
    }

    #[tokio::test]
    async fn fixture_peer_search_hit_is_visible_until_excluded() {
        let port = free_port().await;
        let (session, mut events, mut server) = online(port, MAX_PEERS).await;
        session
            .search(SearchRequest {
                mode: crate::model::SearchMode::Global,
                query: "piano".to_owned(),
                room: String::new(),
                user: String::new(),
            })
            .await;
        let mut frames = FrameDecoder::new();
        let sent = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(sent.code, server::FILE_SEARCH);
        let token = u32::from_le_bytes(sent.payload[0..4].try_into().unwrap());
        let mut peer = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        peer.write_all(&encode_peer_init("bob", ConnType::Peer).unwrap())
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(event, SessionEvent::PeerReady { .. }) {
                break;
            }
        }
        let response = FileSearchResponse {
            username: "bob".to_owned(),
            token,
            files: vec![crate::protocol::SearchResultFile {
                path: "jazz\\piano.mp3".to_owned(),
                size: 9,
                attributes: Vec::new(),
            }],
            free_slot: true,
            upload_speed: 0,
            queue: 1,
            private_files: Vec::new(),
        };
        let wire = encode_frame(
            crate::protocol::peer::FILE_SEARCH_RESPONSE,
            &response.encode().unwrap(),
        )
        .unwrap();
        peer.write_all(&wire).await.unwrap();
        let hit = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::SearchResult(_, hit) = event {
                break hit;
            }
        };
        assert_eq!(hit.path, "jazz\\piano.mp3");
        let mut app = crate::app::App::preview();
        app.set_account("alice");
        app.apply_session(logged_in());
        app.apply_session(SessionEvent::SearchResult(0, hit));
        assert!(
            app.visible_hits()
                .iter()
                .any(|hit| hit.path.contains("piano"))
        );
        app.set_exclude("piano");
        assert!(
            app.visible_hits()
                .iter()
                .all(|hit| !hit.path.contains("piano"))
        );
        drop(session);
    }

    #[tokio::test]
    async fn wishlist_item_is_sent_only_after_the_interval() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.wishlist = vec!["jazz".to_owned(), "piano".to_owned()];
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut frames = FrameDecoder::new();
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let _ = read_server_frame(&mut sock, &mut frames).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let mut sock = ready_rx.await.unwrap();
        assert_socket_quiet(&sock, "wishlist search was sent before the interval").await;
        let interval = encode_frame(server::WISHLIST_INTERVAL, &60u32.to_le_bytes()).unwrap();
        sock.write_all(&interval).await.unwrap();
        let mut frames = FrameDecoder::new();
        let first = read_server_frame(&mut sock, &mut frames).await;
        assert_eq!(first.code, server::WISHLIST_SEARCH);
        assert!(first.payload.windows(4).any(|bytes| bytes == b"jazz"));
        sock.write_all(&interval).await.unwrap();
        let second = read_server_frame(&mut sock, &mut frames).await;
        assert_eq!(second.code, server::WISHLIST_SEARCH);
        assert!(second.payload.windows(5).any(|bytes| bytes == b"piano"));
        drop(session);
        let _ = server.await;
    }

    #[tokio::test]
    async fn incoming_search_answers_a_shared_token_only() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-incoming-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let public = root.join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::write(public.join("notes.txt"), b"hello").unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.shares.public = vec![public];
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut frames = FrameDecoder::new();
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let _ = read_server_frame(&mut sock, &mut frames).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut rx) = mpsc::channel(8);
        let session = Session::spawn(config, tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let mut sock = ready_rx.await.unwrap();
        sock.write_all(&incoming_search("bob", 1, "n"))
            .await
            .unwrap();
        assert_socket_quiet(&sock, "a one-character query produced a response").await;
        sock.write_all(&incoming_search("bob", 2, "notes"))
            .await
            .unwrap();
        let mut frames = FrameDecoder::new();
        let request = read_server_frame(&mut sock, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        sock.write_all(
            &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
        )
        .await
        .unwrap();
        let (mut inbound, _) = listener.accept().await.unwrap();
        let (handshake, remainder) = read_handshake(&mut inbound).await;
        assert!(matches!(handshake, PeerHandshake::PeerInit { .. }));
        let mut peer_frames = FrameDecoder::new();
        peer_frames.push(&remainder);
        let response = read_server_frame(&mut inbound, &mut peer_frames).await;
        assert_eq!(response.code, crate::protocol::peer::FILE_SEARCH_RESPONSE);
        let decoded = FileSearchResponse::decode(&response.payload).unwrap();
        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.token, 2);
        assert!(decoded.files.iter().any(|file| file.path.contains("notes")));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    async fn assert_socket_quiet(sock: &TcpStream, message: &str) {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut buf = [0u8; 64];
        match sock.try_read(&mut buf) {
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Ok(count) => {
                let rest = strip_share_counts(&buf[..count]);
                assert!(rest.is_empty(), "{message}: {rest:?}");
            }
            Err(err) => panic!("{err}"),
        }
    }

    #[tokio::test]
    async fn a_closed_peer_is_asked_again_and_the_download_leaves_the_queue() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-redial-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        let bob = connect_named(port, "bob").await;
        wait_peer(&mut events, "bob").await;
        drop(bob);
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 8,
                folder: None,
                stage: false,
            })
            .await;
        let mut frames = FrameDecoder::new();
        let request = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 7, 8, 0).await;
        let transfer = wait_transfer(&mut events, crate::model::TransferState::Transferring).await;
        assert_eq!(transfer.user, "bob");
        let _ = std::fs::remove_dir_all(root);
        drop(session);
    }

    #[tokio::test]
    async fn fixture_upload_matches_and_resumes_at_1024() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-download-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let payload: Vec<u8> = (0..65536)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 65536,
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 7, 65536, 0).await;
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &payload[..1024]).await;
        drop(file);
        let closed =
            wait_transfer(&mut events, crate::model::TransferState::ConnectionClosed).await;
        assert_eq!(closed.done, 1024);
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 65536,
                folder: None,
                stage: false,
            })
            .await;
        let again = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(again.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(&mut peer, &mut frames, 8, 65536, 1024).await;
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 8, 1024, &payload[1024..]).await;
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, 65536);
        let saved = std::fs::read(download.join("tone.bin")).unwrap();
        assert_eq!(saved, payload);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_connection_closed_download_is_asked_again() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-retry-dl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let payload: Vec<u8> = (0..2048)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: u64::try_from(payload.len()).unwrap(),
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(
            &mut peer,
            &mut frames,
            7,
            u64::try_from(payload.len()).unwrap(),
            0,
        )
        .await;
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &payload[..1024]).await;
        drop(file);
        let closed =
            wait_transfer(&mut events, crate::model::TransferState::ConnectionClosed).await;
        assert_eq!(closed.done, 1024);
        tokio::time::pause();
        tokio::time::advance(RETRY_CONNECTION).await;
        tokio::time::resume();
        let again = wait_transfer(&mut events, crate::model::TransferState::Queued).await;
        assert_eq!(again.done, 1024);
        let asked = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(asked.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(
            &mut peer,
            &mut frames,
            8,
            u64::try_from(payload.len()).unwrap(),
            1024,
        )
        .await;
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_firewalled_upload_is_pulled_after_pierce() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-pierce-dl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let payload = b"tone-bin";
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: u64::try_from(payload.len()).unwrap(),
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_upload(
            &mut peer,
            &mut frames,
            7,
            u64::try_from(payload.len()).unwrap(),
            0,
        )
        .await;
        let file_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let file_port = file_listener.local_addr().unwrap().port();
        let mut connect = Vec::new();
        connect.extend_from_slice(&3u32.to_le_bytes());
        connect.extend_from_slice(b"bob");
        connect.extend_from_slice(&1u32.to_le_bytes());
        connect.extend_from_slice(b"F");
        connect.extend_from_slice(&[0x01, 0x00, 0x00, 0x7f]);
        connect.extend_from_slice(&u32::from(file_port).to_le_bytes());
        connect.extend_from_slice(&9u32.to_le_bytes());
        connect.push(0);
        connect.extend_from_slice(&0u32.to_le_bytes());
        connect.extend_from_slice(&0u32.to_le_bytes());
        server
            .write_all(&encode_frame(server::CONNECT_TO_PEER, &connect).unwrap())
            .await
            .unwrap();
        let (mut inbound, _) = tokio::time::timeout(Duration::from_secs(2), file_listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            read_handshake(&mut inbound).await.0,
            PeerHandshake::PierceFireWall { token: 9 }
        );
        inbound.write_all(&7u32.to_le_bytes()).await.unwrap();
        let mut offset = [0u8; 8];
        inbound.read_exact(&mut offset).await.unwrap();
        assert_eq!(u64::from_le_bytes(offset), 0);
        inbound.write_all(payload).await.unwrap();
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, u64::try_from(payload.len()).unwrap());
        assert_eq!(std::fs::read(download.join("tone.bin")).unwrap(), payload);
        drop(peer);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn third_upload_waits_while_two_slots_are_busy() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-slots-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let public = root.join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::write(public.join("tone.bin"), b"tone-bytes").unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.shares.public = vec![public];
        config.upload_slots = 2;
        let (session, mut events, _server) = started(config, fixture).await;
        let mut bob = connect_named(port, "bob").await;
        wait_peer(&mut events, "bob").await;
        bob.write_all(&crate::protocol::peer::encode_queue_upload("public\\tone.bin").unwrap())
            .await
            .unwrap();
        let mut carol = connect_named(port, "carol").await;
        wait_peer(&mut events, "carol").await;
        carol
            .write_all(&crate::protocol::peer::encode_queue_upload("public\\tone.bin").unwrap())
            .await
            .unwrap();
        let mut dave = connect_named(port, "dave").await;
        wait_peer(&mut events, "dave").await;
        dave.write_all(&crate::protocol::peer::encode_queue_upload("public\\tone.bin").unwrap())
            .await
            .unwrap();
        let mut bob_frames = FrameDecoder::new();
        let mut carol_frames = FrameDecoder::new();
        let mut dave_frames = FrameDecoder::new();
        assert_eq!(
            read_server_frame(&mut bob, &mut bob_frames).await.code,
            crate::protocol::peer::TRANSFER_REQUEST
        );
        assert_eq!(
            read_server_frame(&mut carol, &mut carol_frames).await.code,
            crate::protocol::peer::TRANSFER_REQUEST
        );
        assert_eq!(
            read_server_frame(&mut dave, &mut dave_frames).await.code,
            crate::protocol::peer::PLACE_IN_QUEUE_RESPONSE
        );
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn banned_username_is_denied() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.banned = vec!["eve".to_owned()];
        let (session, mut events, _server) = started(config, fixture).await;
        let mut eve = connect_named(port, "eve").await;
        wait_peer(&mut events, "eve").await;
        eve.write_all(&crate::protocol::peer::encode_queue_upload("public\\tone.bin").unwrap())
            .await
            .unwrap();
        let denied = read_server_frame(&mut eve, &mut FrameDecoder::new()).await;
        assert_eq!(denied.code, crate::protocol::peer::UPLOAD_DENIED);
        assert_eq!(
            crate::protocol::peer::decode_upload_denied(&denied.payload)
                .unwrap()
                .reason,
            crate::protocol::peer::REJECT_BANNED
        );
        drop(session);
    }

    #[tokio::test]
    async fn fixture_browse_of_two_files_builds_two_rows() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        let (session, mut events, mut server) = started(config, fixture).await;
        session.browse("bob".to_owned()).await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let share = read_server_frame(&mut peer, &mut frames).await;
        let info = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(share.code, crate::protocol::peer::SHARED_FILE_LIST_REQUEST);
        assert_eq!(info.code, crate::protocol::peer::USER_INFO_REQUEST);
        let list = crate::protocol::peer::SharedFileListResponse {
            list: vec![crate::protocol::peer::SharedFolder {
                directory: "music".to_owned(),
                files: vec![
                    crate::protocol::peer::SharedFile {
                        name: "a.bin".to_owned(),
                        size: 4,
                        attributes: Vec::new(),
                    },
                    crate::protocol::peer::SharedFile {
                        name: "b.bin".to_owned(),
                        size: 8,
                        attributes: Vec::new(),
                    },
                ],
            }],
            private_list: Vec::new(),
        };
        peer.write_all(
            &encode_frame(
                crate::protocol::peer::SHARED_FILE_LIST_RESPONSE,
                &list.encode().unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        peer.write_all(
            &crate::protocol::peer::encode_user_info_response(
                &crate::protocol::peer::UserInfoResponse {
                    description: "plays piano".to_owned(),
                    picture: None,
                    total_uploads: 3,
                    queue_size: 1,
                    slots_available: true,
                    upload_allowed: 2,
                },
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let mut rows = None;
        let mut described = false;
        for _ in 0..8 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            match event {
                SessionEvent::Browse { rows: got, .. } => rows = Some(got),
                SessionEvent::UserInfo { description, .. } => {
                    assert_eq!(description, "plays piano");
                    described = true;
                }
                _ => {}
            }
            if rows.is_some() && described {
                break;
            }
        }
        let rows = rows.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "a.bin");
        assert_eq!(rows[0].size, Some(4));
        assert_eq!(rows[1].name, "b.bin");
        assert_eq!(rows[1].size, Some(8));
        let mut app = crate::app::App::preview();
        app.bind_config(
            std::env::temp_dir().join(format!("soul-sever-browse-{}", std::process::id())),
            {
                let mut saved = crate::config::Config::default();
                saved.buddies.push(crate::config::Buddy {
                    name: "bob".to_owned(),
                    note: String::new(),
                    notify: false,
                    prioritized: false,
                    trusted: false,
                });
                saved
            },
        );
        app.set_view(crate::app::View::Users);
        let index = app
            .users()
            .iter()
            .position(|user| user.name == "bob")
            .unwrap();
        for _ in 0..index {
            app.on_key(key(KeyCode::Char('j')));
        }
        app.apply_session(SessionEvent::Browse {
            user: "bob".to_owned(),
            rows,
        });
        app.apply_session(SessionEvent::UserInfo {
            user: "bob".to_owned(),
            description: "plays piano".to_owned(),
            total_uploads: 3,
            queue_size: 1,
            slots_available: true,
        });
        assert_eq!(
            app.browse()
                .iter()
                .filter(|row| row.size.is_some() && row.name.ends_with(".bin"))
                .count(),
            2
        );
        assert!(app.user_detail().contains("plays piano"));
        drop(session);
    }

    #[tokio::test]
    async fn banned_peers_search_hit_is_removed_and_upload_is_denied() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-ban-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.save(&path).unwrap();
        let (session, mut events, _server) = started(config, fixture).await;
        let mut app = crate::app::App::preview();
        app.set_account("alice");
        app.apply_session(logged_in());
        app.bind_config(path.clone(), crate::config::Config::load(&path).unwrap());
        app.apply_session(SessionEvent::SearchResult(
            0,
            crate::model::SearchHit {
                user: "eve".to_owned(),
                path: "music\\a.bin".to_owned(),
                size: 4,
                bitrate: None,
                duration: None,
                bit_depth: None,
                sample_rate: None,
                queue: 0,
                free_slot: true,
                upload_speed: 0,
                country: String::new(),
            },
        ));
        app.set_view(crate::app::View::Search);
        app.set_exclude("preview");
        assert!(app.visible_hits().iter().any(|hit| hit.user == "eve"));
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.visible_hits().iter().all(|hit| hit.user != "eve"));
        let (banned, ignored, prioritized) = app.take_lists().unwrap();
        session.set_lists(banned, ignored, prioritized).await;
        let mut eve = connect_named(port, "eve").await;
        wait_peer(&mut events, "eve").await;
        eve.write_all(&crate::protocol::peer::encode_queue_upload("music\\a.bin").unwrap())
            .await
            .unwrap();
        let denied = read_server_frame(&mut eve, &mut FrameDecoder::new()).await;
        assert_eq!(denied.code, crate::protocol::peer::UPLOAD_DENIED);
        assert_eq!(
            crate::protocol::peer::decode_upload_denied(&denied.payload)
                .unwrap()
                .reason,
            crate::protocol::peer::REJECT_BANNED
        );
        let loaded = crate::config::Config::load(&path).unwrap();
        assert_eq!(loaded.banned, vec!["eve".to_owned()]);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn watch_user_tracks_online_away_and_offline() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.buddies.push(crate::config::Buddy {
            name: "bob".to_owned(),
            note: String::new(),
            notify: false,
            prioritized: false,
            trusted: false,
        });
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let online =
            hex("03000000626f6201020000000a00000001000000000000000200000001000000020000005553");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut frames = FrameDecoder::new();
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let watch = read_server_frame(&mut sock, &mut frames).await;
            assert_eq!(watch.code, server::WATCH_USER);
            assert!(watch.payload.windows(3).any(|bytes| bytes == b"bob"));
            let payload = online;
            sock.write_all(&encode_frame(server::WATCH_USER, &payload).unwrap())
                .await
                .unwrap();
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut rx) = mpsc::channel(16);
        let session = Session::spawn(config, tx);
        let watched = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Watched {
                status, country, ..
            } = event
            {
                break (status, country);
            }
        };
        assert_eq!(watched.0, 2);
        assert_eq!(watched.1, "US");
        let mut sock = ready_rx.await.unwrap();
        sock.write_all(&user_status_frame(1)).await.unwrap();
        let away = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::UserStatus { status, .. } = event {
                break status;
            }
        };
        assert_eq!(away, 1);
        sock.write_all(&user_status_frame(0)).await.unwrap();
        let offline = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::UserStatus { status, .. } = event {
                break status;
            }
        };
        assert_eq!(offline, 0);
        drop(session);
    }

    #[tokio::test]
    async fn fixture_joins_preview_music_and_censors_the_line() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-chat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.auto_join = vec!["preview-music".to_owned()];
        config.censor = vec![crate::config::WordPair {
            from: "secret".to_owned(),
            to: "****".to_owned(),
        }];
        config.log_rooms = true;
        config.room_log_dir = root.to_string_lossy().into_owned();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let join_body = join_users("preview-music", "bob");
        let say_body = say_line("preview-music", "bob", "hello secret");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let mut frames = FrameDecoder::new();
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let _ = read_server_frame(&mut sock, &mut frames).await;
            let join = read_server_frame(&mut sock, &mut frames).await;
            assert_eq!(join.code, server::JOIN_ROOM);
            assert!(
                join.payload
                    .windows(13)
                    .any(|bytes| bytes == b"preview-music")
            );
            sock.write_all(&encode_frame(server::JOIN_ROOM, &join_body).unwrap())
                .await
                .unwrap();
            sock.write_all(&encode_frame(server::SAY_CHATROOM, &say_body).unwrap())
                .await
                .unwrap();
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut rx) = mpsc::channel(16);
        let session = Session::spawn(config, tx);
        let text = loop {
            let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Chat(line) = event {
                break line.text;
            }
        };
        assert_eq!(text, "hello ****");
        let mut app = crate::app::App::preview();
        app.apply_session(SessionEvent::Chat(crate::model::ChatLine {
            room: "preview-music".to_owned(),
            time: "00:00".to_owned(),
            user: "bob".to_owned(),
            text: text.clone(),
        }));
        assert!(
            app.chat_for_selected_room()
                .iter()
                .any(|line| line.text == "hello ****")
        );
        assert!(
            app.chat_for_selected_room()
                .iter()
                .any(|line| line.text.contains("local sample"))
        );
        let saved = std::fs::read_to_string(root.join("preview-music.log")).unwrap();
        assert!(saved.contains("hello ****"));
        session
            .say("preview-music".to_owned(), "secret".to_owned())
            .await;
        let mut sock = ready_rx.await.unwrap();
        let said = read_server_frame(&mut sock, &mut FrameDecoder::new()).await;
        assert_eq!(said.code, server::SAY_CHATROOM);
        assert!(!said.payload.windows(6).any(|bytes| bytes == b"secret"));
        assert!(said.payload.windows(4).any(|bytes| bytes == b"****"));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn private_message_is_acked_and_auto_reply_waits_for_away() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.auto_reply = "back later".to_owned();
        let (session, _events, mut server) = started(config, fixture).await;
        server
            .write_all(&encode_frame(server::MESSAGE_USER, &private_line(7, "bob", "hi")).unwrap())
            .await
            .unwrap();
        let mut frames = FrameDecoder::new();
        let ack = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(ack.code, server::MESSAGE_ACKED);
        assert_eq!(&ack.payload[..4], &7u32.to_le_bytes());
        assert!(frames.pop().unwrap().is_none());
        assert_socket_quiet(&server, "auto-reply was sent while online").await;
        session.set_away(true).await;
        let status = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(status.code, server::SET_STATUS);
        assert_eq!(&status.payload[..4], &1i32.to_le_bytes());
        server
            .write_all(&encode_frame(server::MESSAGE_USER, &private_line(8, "bob", "hi")).unwrap())
            .await
            .unwrap();
        let mut frames = FrameDecoder::new();
        let ack = read_server_frame(&mut server, &mut frames).await;
        let reply = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(ack.code, server::MESSAGE_ACKED);
        assert_eq!(&ack.payload[..4], &8u32.to_le_bytes());
        assert_eq!(reply.code, server::MESSAGE_USER);
        assert!(
            reply
                .payload
                .windows(10)
                .any(|bytes| bytes == b"back later")
        );
        drop(session);
    }

    fn pack_string(text: &str) -> Vec<u8> {
        let mut out = u32::try_from(text.len()).unwrap().to_le_bytes().to_vec();
        out.extend_from_slice(text.as_bytes());
        out
    }

    fn join_users(room: &str, user: &str) -> Vec<u8> {
        let mut body = pack_string(room);
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend(pack_string(user));
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&2u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 20]);
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend(pack_string("US"));
        body
    }

    fn say_line(room: &str, user: &str, message: &str) -> Vec<u8> {
        let mut body = pack_string(room);
        body.extend(pack_string(user));
        body.extend(pack_string(message));
        body
    }

    fn private_line(id: u32, user: &str, message: &str) -> Vec<u8> {
        let mut body = id.to_le_bytes().to_vec();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend(pack_string(user));
        body.extend(pack_string(message));
        body.push(1);
        body
    }

    fn user_status_frame(status: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(b"bob");
        payload.extend_from_slice(&status.to_le_bytes());
        payload.push(0);
        encode_frame(server::GET_USER_STATUS, &payload).unwrap()
    }

    #[tokio::test]
    async fn a_moved_download_is_indexed_and_advertised() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-moved-share-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let incomplete = root.join("incomplete");
        let download = root.join("library");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let tagged = root.join("source.wav");
        write_tagged_wav(&tagged);
        let payload = std::fs::read(&tagged).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        let mut frames = FrameDecoder::new();
        let initial = read_any_frame(&mut server, &mut frames).await;
        assert_eq!(initial.code, server::SHARED_FOLDERS_FILES);
        assert_eq!(initial.payload, [0u8; 8]);
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\Dookie\\01.wav".to_owned(),
                size: u64::try_from(payload.len()).unwrap(),
                folder: None,
                stage: false,
            })
            .await;
        let request = read_server_frame(&mut server, &mut frames).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut peer_frames = FrameDecoder::new();
        peer_frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut peer_frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        let request = crate::protocol::peer::TransferRequest {
            direction: crate::protocol::peer::DIRECTION_UPLOAD,
            token: 7,
            file: "music\\Dookie\\01.wav".to_owned(),
            filesize: Some(u64::try_from(payload.len()).unwrap()),
        };
        peer.write_all(&crate::protocol::peer::encode_transfer_request(&request).unwrap())
            .await
            .unwrap();
        let mut response = read_server_frame(&mut peer, &mut peer_frames).await;
        if response.code == crate::protocol::peer::PLACE_IN_QUEUE_REQUEST {
            response = read_server_frame(&mut peer, &mut peer_frames).await;
        }
        assert_eq!(response.code, crate::protocol::peer::TRANSFER_RESPONSE);
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        send_file_part(&mut file, 7, 0, &payload).await;
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, u64::try_from(payload.len()).unwrap());
        let filed = download
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        let filed_len = std::fs::metadata(&filed).unwrap().len();
        assert!(filed_len > 0);
        assert!(!download.join("01.wav").exists());
        let (shared_root, public_files, public_folders) =
            tokio::time::timeout(Duration::from_secs(2), async {
                let mut shared_root = None;
                let mut public_files = None;
                let mut public_folders = None;
                loop {
                    match events.recv().await.unwrap() {
                        SessionEvent::ShareRoot { path } => shared_root = Some(path),
                        SessionEvent::Shares {
                            public_files: files,
                            public_folders: folders,
                            ..
                        } if files > 0 => {
                            public_files = Some(files);
                            public_folders = Some(folders);
                        }
                        _ => {}
                    }
                    if let (Some(path), Some(files), Some(folders)) =
                        (shared_root.clone(), public_files, public_folders)
                    {
                        break (path, files, folders);
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(shared_root, download);
        assert_eq!(public_files, 1);
        assert_eq!(public_folders, 1);
        let announced = loop {
            let frame = read_any_frame(&mut server, &mut frames).await;
            if frame.code == server::SHARED_FOLDERS_FILES {
                break frame;
            }
        };
        let mut counts = Vec::new();
        counts.extend_from_slice(&1u32.to_le_bytes());
        counts.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(announced.payload, counts);

        let mut bob = connect_named(port, "bob").await;
        bob.write_all(&crate::protocol::peer::encode_shared_file_list_request().unwrap())
            .await
            .unwrap();
        let mut bob_frames = FrameDecoder::new();
        let list_frame = read_any_frame(&mut bob, &mut bob_frames).await;
        let list =
            crate::protocol::peer::SharedFileListResponse::decode(&list_frame.payload).unwrap();
        assert!(list.list.iter().any(|folder| {
            folder.directory == "library\\Green Day\\Dookie"
                && folder
                    .files
                    .iter()
                    .any(|file| file.name == "01 Basket Case.wav")
        }));
        assert!(
            list.list
                .iter()
                .all(|folder| { folder.files.iter().all(|file| file.name != "01.wav") })
        );
        let virtual_path = "library\\Green Day\\Dookie\\01 Basket Case.wav";
        bob.write_all(&crate::protocol::peer::encode_queue_upload(virtual_path).unwrap())
            .await
            .unwrap();
        let upload = read_any_frame(&mut bob, &mut bob_frames).await;
        assert_eq!(upload.code, crate::protocol::peer::TRANSFER_REQUEST);
        let asked = crate::protocol::peer::decode_transfer_request(&upload.payload).unwrap();
        assert_eq!(asked.file, virtual_path);
        assert_eq!(asked.filesize, Some(filed_len));
        bob.write_all(&crate::protocol::peer::encode_queue_upload("library\\01.wav").unwrap())
            .await
            .unwrap();
        let denied = read_any_frame(&mut bob, &mut bob_frames).await;
        assert_eq!(denied.code, crate::protocol::peer::UPLOAD_DENIED);
        assert_eq!(
            crate::protocol::peer::decode_upload_denied(&denied.payload)
                .unwrap()
                .reason,
            crate::protocol::peer::REJECT_FILE_NOT_SHARED
        );
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    fn write_tagged_wav(path: &std::path::Path) {
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

    #[tokio::test]
    async fn share_scan_reports_one_public_file() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-shares-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), b"hi").unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.bind_address = "127.0.0.1".to_owned();
        config.shares.public = vec![root.clone()];
        let (session, mut events, _server) = started(config, fixture).await;
        let files = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Shares { public_files, .. } = events.recv().await.unwrap() {
                    break public_files;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(files, 1);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_public_share_is_announced_and_a_buddy_file_stays_hidden() {
        let public_root = std::env::temp_dir().join(format!(
            "soul-sever-public-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let buddy_root = std::env::temp_dir().join(format!(
            "soul-sever-buddy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&public_root).unwrap();
        std::fs::create_dir_all(&buddy_root).unwrap();
        std::fs::write(public_root.join("note.txt"), b"hi").unwrap();
        std::fs::write(buddy_root.join("secret.txt"), b"no").unwrap();
        let public_name = public_root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let buddy_name = buddy_root.file_name().unwrap().to_str().unwrap().to_owned();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.bind_address = "127.0.0.1".to_owned();
        config.shares.public = vec![public_root.clone()];
        config.shares.buddy = vec![buddy_root.clone()];
        config.buddies = vec![crate::config::Buddy {
            name: "carol".to_owned(),
            note: String::new(),
            notify: false,
            prioritized: false,
            trusted: false,
        }];
        let (session, mut events, mut server) = started(config, fixture).await;
        let files = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Shares { public_files, .. } = events.recv().await.unwrap() {
                    break public_files;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(files, 1);
        let mut frames = FrameDecoder::new();
        let announced = loop {
            let frame = read_any_frame(&mut server, &mut frames).await;
            if frame.code == server::SHARED_FOLDERS_FILES {
                break frame;
            }
        };
        let mut counts = Vec::new();
        counts.extend_from_slice(&1u32.to_le_bytes());
        counts.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(announced.payload, counts);

        let mut bob = connect_named(port, "bob").await;
        bob.write_all(&crate::protocol::peer::encode_shared_file_list_request().unwrap())
            .await
            .unwrap();
        let mut bob_frames = FrameDecoder::new();
        let list_frame = read_any_frame(&mut bob, &mut bob_frames).await;
        assert_eq!(
            list_frame.code,
            crate::protocol::peer::SHARED_FILE_LIST_RESPONSE
        );
        let list =
            crate::protocol::peer::SharedFileListResponse::decode(&list_frame.payload).unwrap();
        let browsed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Log(message) = events.recv().await.unwrap() {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(browsed, "User bob is browsing your list of shared files");
        assert!(
            list.list
                .iter()
                .any(|folder| { folder.files.iter().any(|file| file.name == "note.txt") })
        );
        assert!(
            list.list
                .iter()
                .all(|folder| { folder.files.iter().all(|file| file.name != "secret.txt") })
        );

        bob.write_all(&crate::protocol::peer::encode_user_info_request().unwrap())
            .await
            .unwrap();
        let info_frame = read_any_frame(&mut bob, &mut bob_frames).await;
        assert_eq!(info_frame.code, crate::protocol::peer::USER_INFO_RESPONSE);
        let info = crate::protocol::peer::decode_user_info_response(&info_frame.payload).unwrap();
        assert_eq!(info.description, "");
        assert!(info.picture.is_none());
        assert_eq!(info.total_uploads, 0);
        assert_eq!(info.queue_size, 0);
        assert!(info.slots_available);
        assert_eq!(info.upload_allowed, 0);

        bob.write_all(
            &crate::protocol::peer::encode_folder_contents_request(
                &crate::protocol::peer::FolderContentsRequest {
                    token: 7,
                    directory: public_name,
                },
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let folder_frame = read_any_frame(&mut bob, &mut bob_frames).await;
        let folder =
            crate::protocol::peer::decode_folder_contents_response(&folder_frame.payload).unwrap();
        assert_eq!(folder.token, 7);
        assert!(
            folder
                .folders
                .iter()
                .any(|entry| { entry.files.iter().any(|file| file.name == "note.txt") })
        );

        bob.write_all(
            &crate::protocol::peer::encode_folder_contents_request(
                &crate::protocol::peer::FolderContentsRequest {
                    token: 8,
                    directory: buddy_name,
                },
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let hidden_frame = read_any_frame(&mut bob, &mut bob_frames).await;
        let hidden =
            crate::protocol::peer::decode_folder_contents_response(&hidden_frame.payload).unwrap();
        assert_eq!(hidden.token, 8);
        assert!(hidden.folders.iter().all(|entry| entry.files.is_empty()));

        let mut carol = connect_named(port, "carol").await;
        carol
            .write_all(&crate::protocol::peer::encode_shared_file_list_request().unwrap())
            .await
            .unwrap();
        let carol_frame = read_any_frame(&mut carol, &mut FrameDecoder::new()).await;
        let carol_list =
            crate::protocol::peer::SharedFileListResponse::decode(&carol_frame.payload).unwrap();
        assert!(
            carol_list
                .list
                .iter()
                .any(|folder| { folder.files.iter().any(|file| file.name == "note.txt") })
        );
        assert!(
            carol_list
                .list
                .iter()
                .any(|folder| { folder.files.iter().any(|file| file.name == "secret.txt") })
        );

        std::fs::write(public_root.join("second.txt"), b"yo").unwrap();
        session
            .rescan(crate::config::Shares {
                public: vec![public_root.clone()],
                buddy: vec![buddy_root.clone()],
                ..crate::config::Shares::default()
            })
            .await;
        let files = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Shares { public_files, .. } = events.recv().await.unwrap() {
                    break public_files;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(files, 2);
        let again = read_any_frame(&mut server, &mut frames).await;
        assert_eq!(again.code, server::SHARED_FOLDERS_FILES);
        let mut counts = Vec::new();
        counts.extend_from_slice(&1u32.to_le_bytes());
        counts.extend_from_slice(&2u32.to_le_bytes());
        assert_eq!(again.payload, counts);

        session
            .rescan(crate::config::Shares {
                public: vec![public_root.join("missing")],
                ..crate::config::Shares::default()
            })
            .await;
        let failed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::ShareScanFailed { message } = events.recv().await.unwrap() {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert!(!failed.is_empty());
        assert_socket_quiet(&server, "a failed rescan replaced the share count").await;

        drop(session);
        let _ = std::fs::remove_dir_all(&public_root);
        let _ = std::fs::remove_dir_all(&buddy_root);
    }

    #[tokio::test]
    async fn adding_a_share_folder_rescans_that_directory() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-rescan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.bind_address = "127.0.0.1".to_owned();
        let (session, mut events, _server) = started(config, fixture).await;
        let files = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Shares { public_files, .. } = events.recv().await.unwrap() {
                    break public_files;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(files, 0);
        std::fs::write(root.join("note.txt"), b"hi").unwrap();
        session
            .rescan(crate::config::Shares {
                public: vec![root.clone()],
                ..crate::config::Shares::default()
            })
            .await;
        let files = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SessionEvent::Shares { public_files, .. } = events.recv().await.unwrap() {
                    break public_files;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(files, 1);
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn port_map_failure_keeps_the_server_connection() {
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.bind_address = "127.0.0.1".to_owned();
        config.upnp = true;
        config.upnp_gateway = "127.0.0.1:1".to_owned();
        let (session, mut events, mut server) = started(config, fixture).await;
        let failed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    events.recv().await.unwrap(),
                    SessionEvent::PortMapFailed { .. }
                ) {
                    break;
                }
            }
        })
        .await;
        assert!(failed.is_ok());
        server
            .write_all(&encode_frame(server::RELOGGED, &[]).unwrap())
            .await
            .unwrap();
        let kicked = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(events.recv().await.unwrap(), SessionEvent::Kicked) {
                    break;
                }
            }
        })
        .await;
        assert!(kicked.is_ok());
        drop(session);
    }

    #[tokio::test]
    async fn a_saved_download_is_shown_and_asked_again() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-queue-restore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let queue = root.join("queue.toml");
        std::fs::write(
            &queue,
            "[[downloads]]\nuser = \"ada\"\npath = \"done.bin\"\nsize = 4\nstate = \"Finished\"\n\n[[downloads]]\nuser = \"bob\"\npath = \"music\\\\tone.bin\"\nsize = 8\nstate = \"Connection closed\"\n",
        )
        .unwrap();
        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.queue_file = Some(queue);
        let (session, mut events, mut server) = started(config, fixture).await;
        let mut saw_bob = false;
        let mut saw_ada = false;
        for _ in 0..8 {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Transfer(transfer) = event {
                if transfer.user == "bob" {
                    assert_eq!(transfer.state, crate::model::TransferState::Queued);
                    assert_eq!(transfer.path, "music\\tone.bin");
                    saw_bob = true;
                }
                if transfer.user == "ada" {
                    assert_eq!(transfer.state, crate::model::TransferState::Finished);
                    saw_ada = true;
                }
            }
            if saw_bob && saw_ada {
                break;
            }
        }
        assert!(saw_bob && saw_ada);
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        assert!(request.payload.windows(3).any(|bytes| bytes == b"bob"));
        assert!(!request.payload.windows(3).any(|bytes| bytes == b"ada"));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    async fn started(
        config: Config,
        fixture: Fixture,
    ) -> (Session, mpsc::Receiver<SessionEvent>, TcpStream) {
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(config.listen_port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut rx) = mpsc::channel(32);
        let session =
            Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, MAX_PEERS);
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap();
        (session, rx, ready_rx.await.unwrap())
    }

    async fn next_event(events: &mut mpsc::Receiver<SessionEvent>) -> SessionEvent {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if !matches!(event, SessionEvent::Shares { .. }) {
                return event;
            }
        }
    }

    async fn connect_named(port: u16, name: &str) -> TcpStream {
        let mut sock = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        sock.write_all(&encode_peer_init(name, ConnType::Peer).unwrap())
            .await
            .unwrap();
        sock
    }

    async fn wait_peer(events: &mut mpsc::Receiver<SessionEvent>, name: &str) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::PeerReady { username, .. } = event
                && username == name
            {
                return;
            }
        }
    }

    async fn wait_transfer(
        events: &mut mpsc::Receiver<SessionEvent>,
        state: crate::model::TransferState,
    ) -> crate::model::Transfer {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::Transfer(transfer) = event
                && transfer.state == state
            {
                return transfer;
            }
        }
    }

    #[tokio::test]
    async fn a_staged_upgrade_is_not_filed_until_it_is_verified() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let incomplete = root.join("incomplete");
        let download = root.join("download");
        std::fs::create_dir_all(&incomplete).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        let original = download.join("song.wav");
        let count = 44_100 * 2;
        crate::quality::write_wav(
            &original,
            44_100,
            16,
            &crate::quality::tone(44_100, 16_000.0, count),
        );
        tag_wav(&original);
        let better = root.join("better.wav");
        crate::quality::write_wav(
            &better,
            44_100,
            16,
            &crate::quality::tone(44_100, 20_000.0, count),
        );
        tag_wav(&better);
        let payload = std::fs::read(&better).unwrap();

        let fixture = Fixture::bind().await.unwrap();
        let addr = fixture.addr().unwrap();
        let port = free_port().await;
        let mut config = alice(addr);
        config.listen_port = port;
        config.incomplete_dir = incomplete.to_string_lossy().into_owned();
        config.download_dir = download.to_string_lossy().into_owned();
        let reply = encode_frame(server::LOGIN, &success_payload()).unwrap();
        let wait_len = ClientMessage::SetWaitPort(u32::from(port))
            .frame()
            .unwrap()
            .len();
        let status_len = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap()
        .len();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut sock = fixture.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
            let _ = read_exact(&mut sock, wait_len).await;
            let _ = read_exact(&mut sock, status_len).await;
            ready_tx.send(sock).unwrap();
        });
        let (tx, mut events) = mpsc::channel(32);
        let session = Session::start_with_cap(config, tx, CONNECT_TIMEOUT, PING_INTERVAL, 2);
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), events.recv()).await;
        let mut server = ready_rx.await.unwrap();
        let size = u64::try_from(payload.len()).unwrap();
        session
            .download(DownloadRequest {
                user: "bob".to_owned(),
                path: "music\\Song.wav".to_owned(),
                size,
                folder: None,
                stage: true,
            })
            .await;
        let request = read_server_frame(&mut server, &mut FrameDecoder::new()).await;
        assert_eq!(request.code, server::GET_PEER_ADDRESS);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        server
            .write_all(
                &encode_frame(server::GET_PEER_ADDRESS, &address_payload(peer_port)).unwrap(),
            )
            .await
            .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        let (_, remainder) = read_handshake(&mut peer).await;
        let mut frames = FrameDecoder::new();
        frames.push(&remainder);
        let queued = read_server_frame(&mut peer, &mut frames).await;
        assert_eq!(queued.code, crate::protocol::peer::QUEUE_UPLOAD);
        offer_named(&mut peer, &mut frames, 7, "music\\Song.wav", size, 0).await;
        let mut file = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut hello = encode_peer_init("bob", ConnType::File).unwrap();
        hello.extend_from_slice(&7u32.to_le_bytes());
        file.write_all(&hello).await.unwrap();
        let mut got = [0u8; 8];
        file.read_exact(&mut got).await.unwrap();
        let half = payload.len() / 2;
        file.write_all(&payload[..half]).await.unwrap();
        assert!(original.is_file());
        assert!(!download.join("Ada").exists());
        file.write_all(&payload[half..]).await.unwrap();
        drop(file);
        let finished = wait_transfer(&mut events, crate::model::TransferState::Finished).await;
        assert_eq!(finished.done, size);
        let staged = incomplete.join("quality").join("Song.wav");
        assert!(staged.is_file(), "{}", staged.display());
        assert!(original.is_file());
        assert!(!download.join("Ada").exists());
        let measured = crate::quality::analyze_path(&original);
        let filed = crate::quality::replace_with(&download, &original, &measured, &staged).unwrap();
        assert!(!original.exists());
        assert!(filed.starts_with(download.join("Ada").join("Album")));
        drop(session);
        let _ = std::fs::remove_dir_all(root);
    }

    fn tag_wav(path: &std::path::Path) {
        use lofty::config::WriteOptions;
        use lofty::file::{AudioFile, TaggedFileExt};
        use lofty::tag::{Accessor, Tag, TagType};
        let mut file = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist("Ada".to_owned());
        tag.set_album("Album".to_owned());
        tag.set_title("Song".to_owned());
        tag.set_track(1);
        file.insert_tag(tag);
        file.save_to_path(path, WriteOptions::default()).unwrap();
    }

    async fn offer_upload(
        peer: &mut TcpStream,
        frames: &mut FrameDecoder,
        token: u32,
        size: u64,
        offset: u64,
    ) {
        offer_named(peer, frames, token, "music\\tone.bin", size, offset).await;
    }

    async fn offer_named(
        peer: &mut TcpStream,
        frames: &mut FrameDecoder,
        token: u32,
        file: &str,
        size: u64,
        offset: u64,
    ) {
        let request = crate::protocol::peer::TransferRequest {
            direction: crate::protocol::peer::DIRECTION_UPLOAD,
            token,
            file: file.to_owned(),
            filesize: Some(size),
        };
        peer.write_all(&crate::protocol::peer::encode_transfer_request(&request).unwrap())
            .await
            .unwrap();
        let mut response = read_server_frame(peer, frames).await;
        if response.code == crate::protocol::peer::PLACE_IN_QUEUE_REQUEST {
            response = read_server_frame(peer, frames).await;
        }
        assert_eq!(response.code, crate::protocol::peer::TRANSFER_RESPONSE);
        assert_eq!(
            crate::protocol::peer::decode_transfer_response(&response.payload)
                .unwrap()
                .filesize,
            Some(offset)
        );
    }

    async fn send_file_part(file: &mut TcpStream, token: u32, offset: u64, bytes: &[u8]) {
        let mut hello = encode_peer_init("bob", ConnType::File).unwrap();
        hello.extend_from_slice(&token.to_le_bytes());
        file.write_all(&hello).await.unwrap();
        let mut got = [0u8; 8];
        file.read_exact(&mut got).await.unwrap();
        assert_eq!(u64::from_le_bytes(got), offset);
        file.write_all(bytes).await.unwrap();
    }

    fn incoming_search(user: &str, token: u32, query: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(u32::try_from(user.len()).unwrap()).to_le_bytes());
        payload.extend_from_slice(user.as_bytes());
        payload.extend_from_slice(&token.to_le_bytes());
        payload.extend_from_slice(&(u32::try_from(query.len()).unwrap()).to_le_bytes());
        payload.extend_from_slice(query.as_bytes());
        encode_frame(server::FILE_SEARCH, &payload).unwrap()
    }
}
