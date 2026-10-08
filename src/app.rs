//! Terminal application state. Input handling is independent of the screen.

use std::cell::{Cell, Ref, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::Config;
use crate::model::{
    BrowseRow, ChatLine, Direction, ListedUser, Room, RoomPerson, SearchHit, SearchLine,
    SearchMode, SettingGroup, ShareGroup, Transfer, TransferState, UserList, setting_groups,
    share_groups,
};
use crate::panels::Panels;
use crate::preview::{self, rate_at};
use crate::protocol::UserStatus;
use crate::session::SessionEvent;

const HISTORY: usize = 240;
/// How many 200 ms ticks a replacement album search collects before a peer is chosen.
const ALBUM_RETRY_TICKS: u8 = 25;
const SEEDED_TICKS: u64 = 120;
const QUERY_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Dashboard,
    Search,
    Downloads,
    Uploads,
    Browse,
    Chat,
    Users,
    Shares,
    Settings,
    Library,
    Quality,
}

impl View {
    pub const ALL: [Self; 11] = [
        Self::Dashboard,
        Self::Search,
        Self::Downloads,
        Self::Uploads,
        Self::Browse,
        Self::Chat,
        Self::Users,
        Self::Shares,
        Self::Settings,
        Self::Library,
        Self::Quality,
    ];

    pub fn key(self) -> char {
        match self {
            Self::Library => '0',
            Self::Quality => 'a',
            _ => char::from(b'1' + self.index() as u8),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "dash",
            Self::Search => "search",
            Self::Downloads => "down",
            Self::Uploads => "up",
            Self::Browse => "browse",
            Self::Chat => "chat",
            Self::Users => "users",
            Self::Shares => "shares",
            Self::Settings => "setup",
            Self::Library => "play",
            Self::Quality => "spec",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::Search => "search",
            Self::Downloads => "downloads",
            Self::Uploads => "uploads",
            Self::Browse => "browse",
            Self::Chat => "chat",
            Self::Users => "users",
            Self::Shares => "shares",
            Self::Settings => "settings",
            Self::Library => "library",
            Self::Quality => "quality",
        }
    }

    pub fn from_key(key: char) -> Option<Self> {
        Self::ALL.into_iter().find(|view| view.key() == key)
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }

    pub fn cycle(self, forward: bool) -> Self {
        let index = self.index();
        let next = if forward {
            (index + 1) % Self::ALL.len()
        } else {
            (index + Self::ALL.len() - 1) % Self::ALL.len()
        };
        Self::ALL[next]
    }
}

/// Which rows and meter samples the views read.
///
/// [`Feed::Preview`] is the unsigned-in shell: empty lists until login.
/// [`Feed::Live`] starts at login and holds only session data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    Preview,
    Live,
}

/// Which list a scrollbar or wheel event belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollId {
    /// The current view's main cursor. Rooms, queues, search, browse, users, and settings sections.
    List,
    /// People in the selected chat room.
    Members,
    /// Wrapped transcript rows. Pinned to the newest line until the reader moves.
    Transcript,
    /// The session log under the view. Pinned to the newest line until the reader moves.
    Log,
    /// A library column. `0` artists, `1` albums, `2` songs.
    Library(u8),
    /// A quality column. `0` measuring, `1` replacing.
    Quality(u8),
    /// Paths in one share column. `0` public, `1` buddy, `2` trusted.
    Share(u8),
    /// Fields in the open settings section.
    Settings,
    /// The folder browser.
    Picker,
    /// The bind-address popup.
    Bind,
}

/// A widget under the pointer. [`Target::Scroll`] is the list background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    OpenView(View),
    Select(usize),
    FocusQuery,
    CycleMode,
    CycleFilter(u8),
    OpenHelp,
    Picker(usize),
    PickerStay,
    Setting(usize),
    Scroll,
    /// Drag this edge. Expanding moves outward. Dragging back stops at the original size.
    Resize(crate::panels::Split, crate::panels::Edge),
    ToggleLog,
    /// A library column. `0` is artists, `1` is albums, `2` is songs.
    Column(u8, usize),
    /// A quality column. `0` is measuring, `1` is replacing.
    Quality(u8, usize),
    /// The playback seek bar. The column inside its rectangle chooses the playhead.
    Seek,
    /// The library transport. Play is the triangle, pause the two bars, stop the square.
    Play,
    Pause,
    Stop,
    /// A delete confirmation. `true` removes the files.
    Confirm(bool),
    /// A person in the room list. The index is that room's member row.
    Person(usize),
    /// A row in the person card. `0` browses, `1` adds a friend, `2` ignores, `3` bans, `4` closes.
    PersonAction(u8),
    /// The scrollbar track. A press drags the thumb.
    VScroll(ScrollId),
    /// The list body, for the wheel. Row targets painted later still receive clicks.
    Wheel(ScrollId),
}

struct PersonCard {
    name: String,
    action: u8,
}

struct DeletePrompt {
    heading: String,
    detail: String,
    paths: Vec<std::path::PathBuf>,
}

/// Which transport shape is lit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportButton {
    Play,
    Pause,
    Stop,
}

type LibraryScan = (
    u64,
    crate::library::Catalog,
    Vec<std::path::PathBuf>,
    Vec<crate::library::Relocation>,
);

struct LibraryJob {
    handle: std::thread::JoinHandle<LibraryScan>,
    live: std::sync::Arc<std::sync::Mutex<Option<LibraryScan>>>,
}

struct QualityJob {
    rx: std::sync::mpsc::Receiver<(PathBuf, crate::quality::Quality)>,
    handles: Vec<std::thread::JoinHandle<()>>,
    paths: Vec<PathBuf>,
}

struct QualityProbe {
    path: PathBuf,
    artist: String,
    title: String,
    duration: Option<u32>,
    goal: crate::quality::UpgradeGoal,
    hits: Vec<SearchHit>,
    ticks: u8,
}

#[derive(Clone)]
struct InflightUpgrade {
    original: PathBuf,
    artist: String,
    title: String,
    duration: Option<u32>,
    goal: crate::quality::UpgradeGoal,
    user: String,
    virtual_path: String,
    hits: Vec<SearchHit>,
    /// The file has arrived and is being measured. The user is free for another song.
    checking: bool,
}

/// A finished search whose acceptable users are already sending a replacement.
struct HeldUpgrade {
    path: PathBuf,
    artist: String,
    title: String,
    duration: Option<u32>,
    goal: crate::quality::UpgradeGoal,
    hits: Vec<SearchHit>,
}

struct QualityLine {
    label: String,
    detail: Vec<String>,
}

#[derive(Default)]
struct QualityBoard {
    measuring: Vec<QualityLine>,
    replacing: Vec<QualityLine>,
}

impl QualityBoard {
    fn column(&self, column: u8) -> &[QualityLine] {
        if column == 0 {
            &self.measuring
        } else {
            &self.replacing
        }
    }
}

enum VerifyDone {
    Replaced { original: PathBuf },
    Kept { original: PathBuf },
}

fn upgrade_gave_up(state: TransferState) -> bool {
    matches!(
        state,
        TransferState::Filtered
            | TransferState::Banned
            | TransferState::PendingShutdown
            | TransferState::Refused
            | TransferState::UserLoggedOff
            | TransferState::ConnectionTimeout
            | TransferState::FileNotShared
            | TransferState::Cancelled
            | TransferState::Failed
    )
}

/// Which peer replies belong on the results list.
enum ShownSearch {
    /// No search has been sent. Injected results are shown.
    Any,
    /// Enter was pressed. Replies wait until the session assigns a token.
    Waiting,
    /// Only this token is listed.
    Token(u32),
}

pub struct App {
    view: View,
    quit: bool,
    help: bool,
    editing: bool,
    elapsed: u64,
    down: Vec<u64>,
    up: Vec<u64>,
    cursors: [usize; 11],
    catalog: crate::library::Catalog,
    library_job: Option<LibraryJob>,
    quality_job: Option<QualityJob>,
    quality_probe: Option<QualityProbe>,
    quality_inflight: Vec<InflightUpgrade>,
    quality_held: Vec<HeldUpgrade>,
    /// Users who already refused a replacement because their queue was full.
    quality_skipped: HashMap<PathBuf, Vec<String>>,
    quality_verify: Vec<std::thread::JoinHandle<VerifyDone>>,
    pending_probe: Option<String>,
    library_path: Option<std::path::PathBuf>,
    library_epoch: u64,
    library_dirty: bool,
    library_column: u8,
    library_cursors: [usize; 3],
    quality_column: u8,
    quality_cursors: [usize; 2],
    quality_board: RefCell<QualityBoard>,
    quality_dirty: Cell<bool>,
    /// Measurements already running when `r` was pressed are discarded.
    quality_drop_job: bool,
    /// A library scan that started before `r` still carries the old verdicts.
    quality_drop_scan: bool,
    player: crate::playback::Player,
    query: String,
    mode: SearchMode,
    room: String,
    user_target: String,
    exclude: String,
    include: String,
    country_filter: String,
    quality: u8,
    min_bitrate: u32,
    min_duration: u32,
    min_size: u64,
    free_only: bool,
    file_type: u8,
    filter_field: Option<u8>,
    pending: Option<crate::session::SearchRequest>,
    pending_downloads: Vec<crate::session::DownloadRequest>,
    pending_cancel: Vec<(Direction, String, String)>,
    pending_clear_finished: Option<(bool, bool)>,
    pending_folders: Vec<(String, String)>,
    open_folders: HashSet<String>,
    asked_folders: HashSet<String>,
    album_downloads: HashSet<String>,
    album_retry: Option<AlbumRetry>,
    album_retry_queue: Vec<(String, String)>,
    file_retry: Option<FileRetry>,
    file_retry_queue: Vec<FileRetryJob>,
    pending_unavailable: Vec<(String, String)>,
    pending_file_unavailable: Vec<(String, String)>,
    live: Vec<Transfer>,
    parent: Option<String>,
    children: u32,
    notice: String,
    account: String,
    banner: Option<String>,
    listen_ip: Option<String>,
    session: Option<SessionEvent>,
    transfers: Vec<Transfer>,
    hits: Vec<SearchHit>,
    hits_epoch: u64,
    open_epoch: u64,
    search_cache: RefCell<Option<(SearchStamp, Vec<SearchLine>)>>,
    browse: Vec<BrowseRow>,
    rooms: Vec<Room>,
    chat: Vec<ChatLine>,
    users: Vec<ListedUser>,
    browse_base: usize,
    stored: Option<(PathBuf, Config)>,
    pending_browse: Option<String>,
    pending_lists: Option<(Vec<String>, Vec<String>, Vec<String>)>,
    user_info: HashMap<String, UserPane>,
    interests: HashMap<String, (Vec<String>, Vec<String>)>,
    recommendations: Vec<crate::protocol::RatedItem>,
    similar: Vec<crate::protocol::RatedItem>,
    file_stats: HashMap<String, (u32, u32)>,
    members: HashMap<String, Vec<crate::model::RoomPerson>>,
    member_cursor: usize,
    member_focus: bool,
    person: Option<PersonCard>,
    pending_inspect: Option<String>,
    tickers: HashMap<String, String>,
    say_draft: String,
    say_edit: bool,
    pending_say: Option<(String, String)>,
    pending_join: Option<String>,
    pending_presence: Option<bool>,
    idle: std::time::Duration,
    away_sent: bool,
    auto_away_minutes: u32,
    feed: Feed,
    shares: [ShareGroup; 3],
    share_paths: [Vec<String>; 3],
    picker: Option<FolderPicker>,
    bind_picker: Option<BindPicker>,
    path_cursor: usize,
    setting_row: usize,
    setting_draft: Option<String>,
    folder_field: Option<crate::settings::Field>,
    pending_rescan: Option<crate::config::Shares>,
    pending_forget: Vec<std::path::PathBuf>,
    pending_moves: Vec<crate::library::Relocation>,
    confirm: Option<DeletePrompt>,
    pending_prefs: Option<crate::settings::LivePrefs>,
    down_done: u64,
    up_done: u64,
    signing_in: bool,
    sign_user: String,
    sign_password: String,
    sign_field: u8,
    sign_busy: bool,
    pending_sign_in: bool,
    server_label: String,
    panels: Panels,
    shown_search: ShownSearch,
    log_lines: Vec<String>,
    log_open: bool,
    cursor_origins: Cell<CursorOrigins>,
    transcript_origin: usize,
    transcript_pinned: bool,
    log_origin: usize,
    log_pinned: bool,
    /// Selection captured when the scrollbar or wheel last moved a list.
    /// While it still matches, the window stays where that scroll left it.
    scroll_hold: ScrollHold,
    scroll_drag: Option<ScrollDrag>,
}

#[derive(Clone, Copy, Default)]
struct CursorOrigins {
    list: [usize; 11],
    members: usize,
    library: [usize; 3],
    quality: [usize; 2],
    share: [usize; 3],
    settings: usize,
    picker: usize,
    bind: usize,
}

#[derive(Clone, Copy, Default)]
struct ScrollHold {
    list: [Option<usize>; 11],
    members: Option<usize>,
    library: [Option<usize>; 3],
    quality: [Option<usize>; 2],
    share: [Option<usize>; 3],
    settings: Option<usize>,
    picker: Option<usize>,
    bind: Option<usize>,
}

#[derive(Clone, Copy)]
struct ScrollDrag {
    id: ScrollId,
    grab_row: u16,
    origin: usize,
    len: usize,
    viewport: usize,
    track: u16,
}

struct UserPane {
    description: String,
    total_uploads: u32,
    queue_size: u32,
    slots_available: bool,
}

impl App {
    /// Empty shell. No sample rows and no socket.
    pub fn blank() -> Self {
        Self {
            view: View::Dashboard,
            quit: false,
            help: false,
            editing: false,
            elapsed: 0,
            down: vec![0],
            up: vec![0],
            cursors: [0; 11],
            catalog: crate::library::Catalog::default(),
            library_job: None,
            quality_job: None,
            quality_probe: None,
            quality_inflight: Vec::new(),
            quality_held: Vec::new(),
            quality_skipped: HashMap::new(),
            quality_verify: Vec::new(),
            pending_probe: None,
            library_path: None,
            library_epoch: 0,
            library_dirty: false,
            library_column: 0,
            library_cursors: [0; 3],
            quality_column: 0,
            quality_cursors: [0; 2],
            quality_board: RefCell::new(QualityBoard::default()),
            quality_dirty: Cell::new(true),
            quality_drop_job: false,
            quality_drop_scan: false,
            player: crate::playback::Player::new(),
            query: String::new(),
            mode: SearchMode::Global,
            room: String::new(),
            user_target: String::new(),
            exclude: String::new(),
            include: String::new(),
            country_filter: String::new(),
            quality: 0,
            min_bitrate: 0,
            min_duration: 0,
            min_size: 0,
            free_only: false,
            file_type: 0,
            filter_field: None,
            pending: None,
            pending_downloads: Vec::new(),
            pending_cancel: Vec::new(),
            pending_clear_finished: None,
            pending_folders: Vec::new(),
            open_folders: HashSet::new(),
            asked_folders: HashSet::new(),
            album_downloads: HashSet::new(),
            album_retry: None,
            album_retry_queue: Vec::new(),
            file_retry: None,
            file_retry_queue: Vec::new(),
            pending_unavailable: Vec::new(),
            pending_file_unavailable: Vec::new(),
            live: Vec::new(),
            parent: None,
            children: 0,
            notice: String::new(),
            account: String::new(),
            banner: None,
            listen_ip: None,
            session: None,
            transfers: Vec::new(),
            hits: Vec::new(),
            hits_epoch: 0,
            open_epoch: 0,
            search_cache: RefCell::new(None),
            browse: Vec::new(),
            rooms: Vec::new(),
            chat: Vec::new(),
            users: Vec::new(),
            browse_base: 0,
            stored: None,
            pending_browse: None,
            pending_lists: None,
            user_info: HashMap::new(),
            interests: HashMap::new(),
            recommendations: Vec::new(),
            similar: Vec::new(),
            file_stats: HashMap::new(),
            members: HashMap::new(),
            member_cursor: 0,
            member_focus: false,
            person: None,
            pending_inspect: None,
            tickers: HashMap::new(),
            say_draft: String::new(),
            say_edit: false,
            pending_say: None,
            pending_join: None,
            pending_presence: None,
            idle: std::time::Duration::ZERO,
            away_sent: false,
            auto_away_minutes: 0,
            feed: Feed::Preview,
            shares: share_groups(),
            share_paths: [Vec::new(), Vec::new(), Vec::new()],
            picker: None,
            bind_picker: None,
            path_cursor: 0,
            setting_row: 0,
            setting_draft: None,
            folder_field: None,
            pending_rescan: None,
            pending_forget: Vec::new(),
            pending_moves: Vec::new(),
            confirm: None,
            pending_prefs: None,
            down_done: 0,
            up_done: 0,
            signing_in: false,
            sign_user: String::new(),
            sign_password: String::new(),
            sign_field: 0,
            sign_busy: false,
            pending_sign_in: false,
            server_label: "server.slsknet.org:2242".to_owned(),
            panels: Panels::default(),
            shown_search: ShownSearch::Any,
            log_lines: Vec::new(),
            log_open: true,
            cursor_origins: Cell::new(CursorOrigins::default()),
            transcript_origin: 0,
            transcript_pinned: true,
            log_origin: 0,
            log_pinned: true,
            scroll_hold: ScrollHold::default(),
            scroll_drag: None,
        }
    }

    /// Local rows for tests. The binary does not open this catalog.
    pub fn preview() -> Self {
        let mut app = Self::blank();
        let tick = SEEDED_TICKS;
        app.down = (0..tick).map(|i| rate_at(i, 0)).collect();
        app.up = (0..tick).map(|i| rate_at(i, 17)).collect();
        app.transfers = preview::transfers();
        app.hits = preview::search_hits();
        app.browse = preview::browse_rows();
        app.rooms = preview::rooms();
        app.order_rooms();
        app.chat = preview::chat_lines();
        app.users = preview::users();
        app.browse_base = app.browse.len();
        app
    }

    /// First launch, before an account exists. No socket is open.
    pub fn sign_in() -> Self {
        let mut app = Self::blank();
        app.signing_in = true;
        app
    }

    /// Saved account, while the server login is still in progress.
    pub fn connecting(username: &str) -> Self {
        let mut app = Self::blank();
        app.account = username.to_owned();
        app.notice = "signing in…".to_owned();
        app
    }

    /// Appends buddies, ignored names, and banned names from `config`.
    pub fn bind_config(&mut self, path: PathBuf, config: Config) {
        for buddy in &config.buddies {
            self.push_user(ListedUser {
                list: UserList::Buddy,
                name: buddy.name.clone(),
                status: UserStatus::Offline,
                country: "—".to_owned(),
                note: buddy.note.clone(),
                notify: buddy.notify,
                prioritized: buddy.prioritized,
                trusted: buddy.trusted,
                last_seen: "—".to_owned(),
            });
        }
        for name in &config.ignored {
            self.push_user(listed(UserList::Ignored, name));
        }
        for name in &config.banned {
            self.push_user(listed(UserList::Banned, name));
        }
        self.auto_away_minutes = config.auto_away_minutes;
        self.server_label = format!("{}:{}", config.server_host, config.server_port);
        self.share_paths = [
            path_list(&config.shares.public),
            path_list(&config.shares.buddy),
            path_list(&config.shares.trusted),
        ];
        self.stored = Some((path.clone(), config));
        self.library_path = Some(path.with_file_name("library.toml"));
        if let Some(root) = self.download_root()
            && let Some(store) = &self.library_path
            && let Some(saved) = crate::library::Catalog::load(store, &root)
        {
            self.catalog = saved;
            self.clamp_library();
        }
        self.refresh_library();
    }

    pub fn on_tick(&mut self) {
        self.idle = self
            .idle
            .saturating_add(std::time::Duration::from_millis(200));
        self.consider_away();
        self.elapsed = self.elapsed.saturating_add(1);
        let down = self.take_rate(Direction::Download);
        let up = self.take_rate(Direction::Upload);
        push_history(&mut self.down, down);
        push_history(&mut self.up, up);
        self.advance_album_retry();
        self.advance_file_retry();
        self.advance_quality_probe();
    }

    /// How long the event loop waits before the next draw.
    /// Playback redraws often enough for the spectrum to move. Everything else stays on the 200 ms tick.
    pub fn frame_wait(&self) -> std::time::Duration {
        if self.player.is_playing() {
            std::time::Duration::from_millis(33)
        } else {
            std::time::Duration::from_millis(200)
        }
    }

    /// Moves a song that has no output device. A live device advances itself.
    pub fn advance_playback(&mut self, elapsed: std::time::Duration) {
        self.player.advance(elapsed);
    }

    /// Bytes newly completed since the previous tick, scaled to one second.
    fn take_rate(&mut self, direction: Direction) -> u64 {
        let done: u64 = self
            .transfers
            .iter()
            .chain(self.live.iter())
            .filter(|transfer| transfer.direction == direction)
            .map(|transfer| transfer.done)
            .sum();
        let previous = match direction {
            Direction::Download => &mut self.down_done,
            Direction::Upload => &mut self.up_done,
        };
        let delta = done.saturating_sub(*previous);
        *previous = done;
        delta.saturating_mul(5)
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.note_activity();
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                self.help = false;
            }
            return;
        }
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => self.commit_delete(),
                KeyCode::Esc | KeyCode::Char('n' | 'q') => self.confirm = None,
                _ => {}
            }
            return;
        }
        if self.person.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.person = None,
                KeyCode::Char('j') | KeyCode::Down => self.nudge_person(true),
                KeyCode::Char('k') | KeyCode::Up => self.nudge_person(false),
                KeyCode::Enter => self.run_person_action(),
                _ => {}
            }
            return;
        }
        if self.signing_in && self.banner.is_none() {
            self.edit_sign_in(key);
            return;
        }
        if self.filter_field.is_some() {
            self.edit_filter(key);
            return;
        }
        if self.picker.is_some() {
            self.edit_picker(key);
            return;
        }
        if self.bind_picker.is_some() {
            self.edit_bind_picker(key);
            return;
        }
        if self.setting_draft.is_some() {
            self.edit_setting(key);
            return;
        }
        if self.editing {
            self.edit(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') if self.view != View::Chat => self.quit = true,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Char('`') => self.log_open = !self.log_open,
            KeyCode::Tab => self.set_view(self.view.cycle(true)),
            KeyCode::BackTab => self.set_view(self.view.cycle(false)),
            KeyCode::Char(ch @ '0'..='9') => {
                if let Some(view) = View::from_key(ch) {
                    self.set_view(view);
                }
            }
            KeyCode::Char('a') if self.view != View::Chat => {
                if let Some(view) = View::from_key('a') {
                    self.set_view(view);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.view == View::Settings => {
                self.nudge(true);
                self.setting_row = 0;
            }
            KeyCode::Char('k') | KeyCode::Up if self.view == View::Settings => {
                self.nudge(false);
                self.setting_row = 0;
            }
            KeyCode::Char('[') if self.view == View::Settings => self.nudge_setting(false),
            KeyCode::Char(']') if self.view == View::Settings => self.nudge_setting(true),
            KeyCode::Enter if self.view == View::Settings => self.activate_setting(),
            KeyCode::Char('x') if self.view == View::Settings => self.clear_setting(),
            KeyCode::Char('j') | KeyCode::Down if self.view == View::Library => {
                self.nudge_library(true);
            }
            KeyCode::Char('k') | KeyCode::Up if self.view == View::Library => {
                self.nudge_library(false);
            }
            KeyCode::Char('[') if self.view == View::Library => self.shift_library(false),
            KeyCode::Char(']') if self.view == View::Library => self.shift_library(true),
            KeyCode::Enter if self.view == View::Library => self.play_library(),
            KeyCode::Char(' ') if self.view == View::Library => self.player.toggle(),
            KeyCode::Char('s') if self.view == View::Library => self.player.stop(),
            KeyCode::Char('x') | KeyCode::Delete if self.view == View::Library => self.ask_delete(),
            KeyCode::Char('j') | KeyCode::Down if self.view == View::Quality => {
                self.nudge_quality(true);
            }
            KeyCode::Char('k') | KeyCode::Up if self.view == View::Quality => {
                self.nudge_quality(false);
            }
            KeyCode::Char('[') if self.view == View::Quality => self.shift_quality(false),
            KeyCode::Char(']') if self.view == View::Quality => self.shift_quality(true),
            KeyCode::Char('r') if self.view == View::Quality => self.recheck_quality(),
            KeyCode::Char(ch)
                if self.view == View::Chat
                    && ch != '['
                    && ch != ']'
                    && ch != '/'
                    && !ch.is_control() =>
            {
                self.begin_say(ch);
            }
            KeyCode::Char('h') | KeyCode::Left => self.set_view(self.view.cycle(false)),
            KeyCode::Char('l') | KeyCode::Right => self.set_view(self.view.cycle(true)),
            KeyCode::Char('j') | KeyCode::Down if self.view == View::Chat && self.member_focus => {
                self.nudge_member(true);
            }
            KeyCode::Char('k') | KeyCode::Up if self.view == View::Chat && self.member_focus => {
                self.nudge_member(false);
            }
            KeyCode::Char('j') | KeyCode::Down => self.nudge(true),
            KeyCode::Char('k') | KeyCode::Up => self.nudge(false),
            KeyCode::Char('/') if self.view == View::Search || self.view == View::Chat => {
                self.editing = true;
                self.say_edit = self.view == View::Chat;
                self.notice.clear();
            }
            KeyCode::Char('m') if self.view == View::Search => {
                self.mode = self.mode.cycle();
            }
            KeyCode::Char(']') if self.view == View::Search => self.cycle_filter(0),
            KeyCode::Enter if self.view == View::Search => {
                if self.selected_is_folder() {
                    self.toggle_folder(self.cursor());
                } else {
                    self.submit_search();
                }
            }
            KeyCode::Char('[') if self.view == View::Chat => self.member_focus = false,
            KeyCode::Char(']') if self.view == View::Chat => self.member_focus = true,
            KeyCode::Enter if self.view == View::Chat && self.member_focus => self.open_person(),
            KeyCode::Enter if self.view == View::Chat => self.request_join(),
            KeyCode::Char('d') if self.view == View::Search || self.view == View::Browse => {
                self.queue_download();
            }
            KeyCode::Char('n') if self.view == View::Users => self.toggle_flag(Flag::Notify),
            KeyCode::Char('p') if self.view == View::Users => self.toggle_flag(Flag::Priority),
            KeyCode::Char('t') if self.view == View::Users => self.toggle_flag(Flag::Trusted),
            KeyCode::Enter if self.view == View::Users || self.view == View::Browse => {
                self.request_browse();
            }
            KeyCode::Enter if self.view == View::Shares => self.open_share_picker(),
            KeyCode::Char('[') if self.view == View::Shares => self.nudge_share_path(false),
            KeyCode::Char(']') if self.view == View::Shares => self.nudge_share_path(true),
            KeyCode::Char('x')
                if matches!(self.view, View::Dashboard | View::Downloads | View::Uploads) =>
            {
                self.remove_transfer();
            }
            KeyCode::Char('X')
                if matches!(self.view, View::Dashboard | View::Downloads | View::Uploads) =>
            {
                self.clear_finished();
            }
            KeyCode::Char('x') if self.view == View::Shares => self.remove_share_folder(),
            KeyCode::Char('x') if self.view == View::Users || self.view == View::Search => {
                self.block_selected(UserList::Banned);
            }
            KeyCode::Char('i') if self.view == View::Users || self.view == View::Search => {
                self.block_selected(UserList::Ignored);
            }
            KeyCode::Char(ch)
                if self.view == View::Search
                    && !ch.is_control()
                    && self.query.chars().count() < QUERY_LIMIT =>
            {
                self.editing = true;
                self.say_edit = false;
                self.query.push(ch);
                self.notice.clear();
            }
            _ => {}
        }
    }

    pub fn on_pointer(&mut self, target: Target) {
        self.note_activity();
        if self.confirm.is_some() {
            match target {
                Target::Confirm(true) => self.commit_delete(),
                Target::Confirm(false) => self.confirm = None,
                Target::PickerStay => {}
                _ => self.confirm = None,
            }
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.picker.is_some() {
            match target {
                Target::Picker(index) => self.activate_picker_row(index),
                Target::PickerStay => {}
                _ => self.picker = None,
            }
            return;
        }
        if self.bind_picker.is_some() {
            match target {
                Target::Picker(index) => self.choose_bind(index),
                Target::PickerStay => {}
                _ => self.bind_picker = None,
            }
            return;
        }
        if self.signing_in && self.banner.is_none() {
            if let Target::Select(index) = target {
                self.sign_field = u8::try_from(index).unwrap_or(1).min(1);
            }
            return;
        }
        if self.person.is_some() {
            match target {
                Target::PersonAction(index) => {
                    if let Some(card) = &mut self.person {
                        card.action = index;
                    }
                    self.run_person_action();
                }
                Target::PickerStay => {}
                _ => self.person = None,
            }
            return;
        }
        match target {
            Target::OpenView(view) => self.set_view(view),
            Target::Select(index) => {
                self.select_row(index);
                if self.view == View::Search {
                    self.toggle_folder(index);
                }
                if self.view == View::Chat {
                    self.member_focus = false;
                    self.request_join();
                }
            }
            Target::Person(index) => self.open_person_at(index),
            Target::PersonAction(_) => {}
            Target::Setting(index) => {
                if self.view == View::Settings {
                    self.setting_row = index;
                    self.activate_setting();
                }
            }
            Target::FocusQuery => {
                if self.view == View::Search {
                    self.editing = true;
                    self.filter_field = None;
                    self.notice.clear();
                }
                if self.view == View::Chat {
                    self.editing = true;
                    self.say_edit = true;
                    self.notice.clear();
                }
            }
            Target::CycleMode => {
                if self.view == View::Search {
                    self.mode = self.mode.cycle();
                }
            }
            Target::CycleFilter(index) => {
                if self.view == View::Search {
                    self.cycle_filter(index);
                }
            }
            Target::OpenHelp => self.help = true,
            Target::ToggleLog => self.log_open = !self.log_open,
            Target::Picker(_) | Target::PickerStay => {}
            Target::Column(column, index) => {
                if self.view == View::Library {
                    self.focus_library(column, index);
                    if column == 2 {
                        self.play_library();
                    }
                }
            }
            Target::Quality(column, index) => {
                if self.view == View::Quality {
                    self.focus_quality(column, index);
                }
            }
            Target::Seek | Target::Confirm(_) => {}
            Target::Play => {
                if self.view == View::Library {
                    self.press_play();
                }
            }
            Target::Pause => {
                if self.view == View::Library {
                    self.player.pause();
                }
            }
            Target::Stop => {
                if self.view == View::Library {
                    self.player.stop();
                }
            }
            Target::Scroll | Target::Resize(_, _) | Target::VScroll(_) | Target::Wheel(_) => {}
        }
    }

    pub fn focus_quality(&mut self, column: u8, index: usize) {
        self.quality_column = column.min(1);
        let len = self.quality_len(self.quality_column);
        if len == 0 {
            return;
        }
        self.quality_cursors[Self::quality_slot(self.quality_column)] = index.min(len - 1);
    }

    pub fn quality_column(&self) -> u8 {
        self.quality_column
    }

    pub fn quality_index(&self, column: u8) -> usize {
        self.quality_cursors[Self::quality_slot(column)]
    }

    fn quality_slot(column: u8) -> usize {
        usize::from(column.min(1))
    }

    pub fn focus_library(&mut self, column: u8, index: usize) {
        if self.confirm.is_some() {
            return;
        }
        self.library_column = column.min(2);
        let len = self.column_len(self.library_column);
        if len == 0 {
            return;
        }
        self.library_cursors[usize::from(self.library_column)] = index.min(len - 1);
    }

    pub fn begin_resize(
        &mut self,
        split: crate::panels::Split,
        edge: crate::panels::Edge,
        x: u16,
        y: u16,
    ) {
        self.panels.begin(split, edge, x, y);
    }

    pub fn resize_to(&mut self, x: u16, y: u16) -> bool {
        self.panels.drag_to(x, y)
    }

    pub fn end_resize(&mut self) {
        self.panels.end();
    }

    pub fn chat_widths(&self, span: u16) -> (u16, u16, u16) {
        let (rooms, members) = self.panels.chat(span);
        let text = span.saturating_sub(rooms).saturating_sub(members);
        (rooms, text, members)
    }

    pub fn search_query_height(&self, span: u16) -> u16 {
        self.panels.search_query(span)
    }

    pub fn search_filters_width(&self, span: u16) -> u16 {
        self.panels.search_filters(span)
    }

    pub fn users_detail_width(&self, span: u16) -> u16 {
        self.panels.users_detail(span)
    }

    pub fn settings_nav_width(&self, span: u16) -> u16 {
        self.panels.settings_nav(span)
    }

    pub fn dash_meters_height(&self, span: u16) -> u16 {
        self.panels.dash_meters(span)
    }

    pub fn dash_columns(&self, span: u16) -> (u16, u16, u16) {
        self.panels.dash_columns(span)
    }

    pub fn share_widths(&self, span: u16) -> (u16, u16, u16) {
        self.panels.shares(span)
    }

    fn keeps_result(&self, token: u32) -> bool {
        match self.shown_search {
            ShownSearch::Any => true,
            ShownSearch::Waiting => false,
            ShownSearch::Token(current) => current == token,
        }
    }

    /// Moves the highlighted row. A drag uses this so an album stays open or closed.
    pub fn select_row(&mut self, index: usize) {
        self.note_activity();
        if self.help
            || self.confirm.is_some()
            || self.picker.is_some()
            || self.bind_picker.is_some()
            || (self.signing_in && self.banner.is_none())
        {
            return;
        }
        self.select(index);
        if self.view == View::Settings {
            self.setting_row = 0;
        }
    }

    pub fn on_scroll(&mut self, down: bool) {
        if self.help || self.confirm.is_some() || self.signing_in() {
            return;
        }
        let id = if self.picker.is_some() {
            ScrollId::Picker
        } else if self.bind_picker.is_some() {
            ScrollId::Bind
        } else if self.view == View::Library {
            ScrollId::Library(self.library_column)
        } else if self.view == View::Quality {
            ScrollId::Quality(self.quality_column)
        } else {
            ScrollId::List
        };
        self.scroll_by(id, down, None);
    }

    /// First visible row for `id`, given the content length and the pane height from the last layout.
    pub fn window_origin(&self, id: ScrollId, len: usize, viewport: usize) -> usize {
        match id {
            ScrollId::Transcript => crate::ui::scroll::pinned_origin(
                self.transcript_origin,
                self.transcript_pinned,
                len,
                viewport,
            ),
            ScrollId::Log => {
                crate::ui::scroll::pinned_origin(self.log_origin, self.log_pinned, len, viewport)
            }
            ScrollId::Share(group) => {
                let group = usize::from(group.min(2));
                let id = ScrollId::Share(u8::try_from(group).unwrap_or(0));
                let stored = self.stored_origin(id);
                let selected = self.view == View::Shares && self.cursor() == group;
                if selected && !self.scroll_held(id) {
                    self.follow_origin(id, stored, self.path_cursor, len, viewport)
                } else {
                    clamped_origin(stored, len, viewport)
                }
            }
            other => {
                let stored = self.stored_origin(other);
                if self.scroll_held(other) {
                    clamped_origin(stored, len, viewport)
                } else {
                    self.follow_origin(other, stored, self.scroll_cursor(other), len, viewport)
                }
            }
        }
    }

    /// Moves the window one row. The selection stays where it is.
    pub fn scroll_by(
        &mut self,
        id: ScrollId,
        down: bool,
        metrics: Option<crate::ui::pointer::ScrollMetrics>,
    ) {
        match id {
            ScrollId::Transcript => self.nudge_pinned(true, down, metrics),
            ScrollId::Log => self.nudge_pinned(false, down, metrics),
            other => self.scroll_window(other, down, metrics),
        }
    }

    fn scroll_window(
        &mut self,
        id: ScrollId,
        down: bool,
        metrics: Option<crate::ui::pointer::ScrollMetrics>,
    ) {
        let Some(metrics) = metrics else {
            return;
        };
        let origin = self.window_origin(id, metrics.len, metrics.viewport);
        let next = step_origin(origin, metrics.len, metrics.viewport, down);
        self.write_origin(id, next, metrics.len, metrics.viewport);
    }

    /// Starts a thumb drag. A press outside the thumb pages one viewport first.
    pub fn begin_scroll(
        &mut self,
        id: ScrollId,
        row: u16,
        metrics: crate::ui::pointer::ScrollMetrics,
    ) {
        let displayed = self.window_origin(id, metrics.len, metrics.viewport);
        let local = row.saturating_sub(metrics.track.y);
        let origin = crate::ui::scroll::page_toward(
            displayed,
            metrics.len,
            metrics.viewport,
            metrics.track.height,
            local,
        );
        self.write_origin(id, origin, metrics.len, metrics.viewport);
        self.scroll_drag = Some(ScrollDrag {
            id,
            grab_row: row,
            origin,
            len: metrics.len,
            viewport: metrics.viewport,
            track: metrics.track.height,
        });
    }

    /// Updates the window from the grab. Returns false when no drag is active.
    pub fn drag_scroll(&mut self, row: u16) -> bool {
        let Some(drag) = self.scroll_drag else {
            return false;
        };
        let delta = i32::from(row) - i32::from(drag.grab_row);
        let next = crate::ui::scroll::origin_after_drag(
            drag.origin,
            drag.len,
            drag.viewport,
            drag.track,
            delta,
        );
        self.write_origin(drag.id, next, drag.len, drag.viewport);
        true
    }

    pub fn end_scroll(&mut self) {
        self.scroll_drag = None;
    }

    fn stored_origin(&self, id: ScrollId) -> usize {
        let origins = self.cursor_origins.get();
        match id {
            ScrollId::List => origins.list[self.view.index()],
            ScrollId::Members => origins.members,
            ScrollId::Library(column) => origins.library[usize::from(column.min(2))],
            ScrollId::Quality(column) => origins.quality[Self::quality_slot(column)],
            ScrollId::Share(group) => origins.share[usize::from(group.min(2))],
            ScrollId::Settings => origins.settings,
            ScrollId::Picker => origins.picker,
            ScrollId::Bind => origins.bind,
            ScrollId::Transcript => self.transcript_origin,
            ScrollId::Log => self.log_origin,
        }
    }

    /// Remembers the window that keeps the selection on screen, without treating it as a scroll.
    fn follow_origin(
        &self,
        id: ScrollId,
        stored: usize,
        cursor: usize,
        len: usize,
        viewport: usize,
    ) -> usize {
        let origin = crate::ui::scroll::reveal(stored, cursor, len, viewport);
        if origin != stored {
            self.set_cursor_origin(id, origin);
        }
        origin
    }

    fn set_cursor_origin(&self, id: ScrollId, origin: usize) {
        let mut origins = self.cursor_origins.get();
        match id {
            ScrollId::List => origins.list[self.view.index()] = origin,
            ScrollId::Members => origins.members = origin,
            ScrollId::Library(column) => {
                origins.library[usize::from(column.min(2))] = origin;
            }
            ScrollId::Quality(column) => {
                origins.quality[Self::quality_slot(column)] = origin;
            }
            ScrollId::Share(group) => origins.share[usize::from(group.min(2))] = origin,
            ScrollId::Settings => origins.settings = origin,
            ScrollId::Picker => origins.picker = origin,
            ScrollId::Bind => origins.bind = origin,
            ScrollId::Transcript | ScrollId::Log => return,
        }
        self.cursor_origins.set(origins);
    }

    fn set_list_origin(&self, slot: usize, origin: usize) {
        let mut origins = self.cursor_origins.get();
        origins.list[slot] = origin;
        self.cursor_origins.set(origins);
    }

    fn scroll_cursor(&self, id: ScrollId) -> usize {
        match id {
            ScrollId::List => self.cursor(),
            ScrollId::Members => self.member_cursor,
            ScrollId::Library(column) => self.library_cursors[usize::from(column.min(2))],
            ScrollId::Quality(column) => self.quality_cursors[Self::quality_slot(column)],
            ScrollId::Share(_) => self.path_cursor,
            ScrollId::Settings => self.setting_row,
            ScrollId::Picker => self
                .picker
                .as_ref()
                .map(|picker| picker.cursor)
                .unwrap_or(0),
            ScrollId::Bind => self
                .bind_picker
                .as_ref()
                .map(|picker| picker.cursor)
                .unwrap_or(0),
            ScrollId::Transcript | ScrollId::Log => 0,
        }
    }

    fn write_origin(&mut self, id: ScrollId, origin: usize, len: usize, viewport: usize) {
        let origin = if viewport == 0 || len <= viewport {
            0
        } else {
            origin.min(len - viewport)
        };
        match id {
            ScrollId::List
            | ScrollId::Members
            | ScrollId::Library(_)
            | ScrollId::Quality(_)
            | ScrollId::Share(_)
            | ScrollId::Settings
            | ScrollId::Picker
            | ScrollId::Bind => self.set_cursor_origin(id, origin),
            ScrollId::Transcript => {
                self.transcript_origin = origin;
                self.transcript_pinned = viewport > 0 && origin.saturating_add(viewport) >= len;
            }
            ScrollId::Log => {
                self.log_origin = origin;
                self.log_pinned = viewport > 0 && origin.saturating_add(viewport) >= len;
            }
        }
        self.note_scroll_hold(id);
    }

    fn note_scroll_hold(&mut self, id: ScrollId) {
        let cursor = self.scroll_cursor(id);
        match id {
            ScrollId::List => self.scroll_hold.list[self.view.index()] = Some(cursor),
            ScrollId::Members => self.scroll_hold.members = Some(cursor),
            ScrollId::Library(column) => {
                self.scroll_hold.library[usize::from(column.min(2))] = Some(cursor);
            }
            ScrollId::Quality(column) => {
                self.scroll_hold.quality[Self::quality_slot(column)] = Some(cursor);
            }
            ScrollId::Share(group) => {
                self.scroll_hold.share[usize::from(group.min(2))] = Some(cursor);
            }
            ScrollId::Settings => self.scroll_hold.settings = Some(cursor),
            ScrollId::Picker => self.scroll_hold.picker = Some(cursor),
            ScrollId::Bind => self.scroll_hold.bind = Some(cursor),
            ScrollId::Transcript | ScrollId::Log => {}
        }
    }

    fn scroll_held(&self, id: ScrollId) -> bool {
        let cursor = self.scroll_cursor(id);
        let held = match id {
            ScrollId::List => self.scroll_hold.list[self.view.index()],
            ScrollId::Members => self.scroll_hold.members,
            ScrollId::Library(column) => self.scroll_hold.library[usize::from(column.min(2))],
            ScrollId::Quality(column) => self.scroll_hold.quality[Self::quality_slot(column)],
            ScrollId::Share(group) => self.scroll_hold.share[usize::from(group.min(2))],
            ScrollId::Settings => self.scroll_hold.settings,
            ScrollId::Picker => self.scroll_hold.picker,
            ScrollId::Bind => self.scroll_hold.bind,
            ScrollId::Transcript | ScrollId::Log => None,
        };
        held == Some(cursor)
    }

    fn nudge_pinned(
        &mut self,
        transcript: bool,
        down: bool,
        metrics: Option<crate::ui::pointer::ScrollMetrics>,
    ) {
        let Some(metrics) = metrics else {
            return;
        };
        let (stored, pinned) = if transcript {
            (self.transcript_origin, self.transcript_pinned)
        } else {
            (self.log_origin, self.log_pinned)
        };
        let origin =
            crate::ui::scroll::pinned_origin(stored, pinned, metrics.len, metrics.viewport);
        let next = step_origin(origin, metrics.len, metrics.viewport, down);
        let at_end = metrics.viewport == 0
            || metrics.len <= metrics.viewport
            || next.saturating_add(metrics.viewport) >= metrics.len;
        if transcript {
            self.transcript_origin = next;
            self.transcript_pinned = at_end;
        } else {
            self.log_origin = next;
            self.log_pinned = at_end;
        }
    }

    fn reset_room_scroll(&mut self) {
        self.member_cursor = 0;
        self.set_cursor_origin(ScrollId::Members, 0);
        self.scroll_hold.members = None;
        self.transcript_origin = 0;
        self.transcript_pinned = true;
    }

    pub fn set_view(&mut self, view: View) {
        self.view = view;
        self.editing = false;
        self.confirm = None;
        self.picker = None;
        self.bind_picker = None;
        self.setting_draft = None;
        self.folder_field = None;
        self.panels.end();
        self.notice.clear();
        if view == View::Library {
            self.library_dirty = true;
            self.start_library_scan();
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn help(&self) -> bool {
        self.help
    }

    pub fn editing(&self) -> bool {
        self.editing
    }

    pub fn saying(&self) -> bool {
        self.editing && self.say_edit
    }

    pub fn say_draft(&self) -> &str {
        &self.say_draft
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn uptime(&self) -> String {
        let secs = self.elapsed / 5;
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn mode(&self) -> SearchMode {
        self.mode
    }

    pub fn notice(&self) -> &str {
        &self.notice
    }

    pub fn log_open(&self) -> bool {
        self.log_open
    }

    pub fn log_lines(&self) -> &[String] {
        &self.log_lines
    }

    fn push_log(&mut self, message: impl AsRef<str>) {
        let stamp = chrono::Local::now().format("%H:%M:%S");
        self.log_lines
            .push(format!("{stamp}  {}", message.as_ref()));
        let extra = self.log_lines.len().saturating_sub(200);
        if extra > 0 {
            self.log_lines.drain(..extra);
        }
    }

    pub fn set_account(&mut self, username: impl Into<String>) {
        self.account = username.into();
    }

    /// Stores the latest server outcome. A failed login also fills [`Self::notice`]
    /// and returns to the sign-in form. A later `Listening` event keeps the login banner.
    pub fn apply_session(&mut self, event: SessionEvent) {
        match &event {
            SessionEvent::LoggedIn { banner, .. } => {
                self.banner = Some(banner.clone());
                self.signing_in = false;
                self.sign_busy = false;
                self.sign_password.clear();
                self.notice.clear();
                self.enter_live();
            }
            SessionEvent::LoginFailed { reason } => {
                self.banner = None;
                self.listen_ip = None;
                self.open_sign_in();
                self.notice = reason.clone();
                self.push_log(reason);
            }
            SessionEvent::TimedOut => {
                self.banner = None;
                self.listen_ip = None;
                self.open_sign_in();
                self.notice = "login timed out".to_owned();
                self.push_log("login timed out");
            }
            SessionEvent::Disconnected { message } => {
                self.sign_busy = false;
                self.notice.clone_from(message);
                self.push_log(message);
                if !message.ends_with("reconnecting") {
                    self.banner = None;
                    self.listen_ip = None;
                    if self.feed != Feed::Live {
                        self.open_sign_in();
                    }
                }
            }
            SessionEvent::ListenFailed { message } => {
                self.listen_ip = None;
                self.notice = message.clone();
                self.push_log(message);
            }
            SessionEvent::Kicked => {
                self.banner = None;
                self.listen_ip = None;
                self.notice = "logged in elsewhere".to_owned();
                self.push_log("Someone logged in to your Soulseek account elsewhere");
            }
            SessionEvent::Listening { port, address } => {
                if address.is_empty() {
                    self.listen_ip = None;
                    self.push_log(format!("Listening on port: {port}"));
                } else {
                    self.listen_ip = Some(address.clone());
                    self.push_log(format!("Listening on {address}:{port}"));
                }
            }
            SessionEvent::PeerReady { .. } => {}
            SessionEvent::PeerFailed { username, message } => {
                let line = format!("{username}: {message}");
                self.notice.clone_from(&line);
                self.push_log(&line);
                return;
            }
            SessionEvent::Log(message) => {
                self.push_log(message);
                return;
            }
            SessionEvent::UserStatus { user, status, .. } => {
                self.note_status(user, *status, None);
                self.note_person(user, Some(*status), None, None, None);
                return;
            }
            SessionEvent::Watched {
                user,
                status,
                country,
            } => {
                self.note_status(user, *status, Some(country));
                self.note_person(user, Some(*status), Some(country), None, None);
                return;
            }
            SessionEvent::Browse { user, rows } => {
                self.browse.truncate(self.browse_base);
                self.browse.extend(rows.iter().cloned());
                self.user_target.clone_from(user);
                return;
            }
            SessionEvent::UserInfo {
                user,
                description,
                total_uploads,
                queue_size,
                slots_available,
            } => {
                self.user_info.insert(
                    user.clone(),
                    UserPane {
                        description: description.clone(),
                        total_uploads: *total_uploads,
                        queue_size: *queue_size,
                        slots_available: *slots_available,
                    },
                );
                return;
            }
            SessionEvent::Interests { user, likes, hates } => {
                self.interests
                    .insert(user.clone(), (likes.clone(), hates.clone()));
                return;
            }
            SessionEvent::Recommendations(items) => {
                self.recommendations.clone_from(items);
                return;
            }
            SessionEvent::SimilarUsers(items) => {
                self.similar.clone_from(items);
                return;
            }
            SessionEvent::UserStats { user, files, dirs } => {
                self.file_stats.insert(user.clone(), (*files, *dirs));
                self.note_person(user, None, None, Some(*files), Some(*dirs));
                return;
            }
            SessionEvent::Chat(line) => {
                self.chat.push(line.clone());
                return;
            }
            SessionEvent::RoomMembers { room, members } => {
                let count = u32::try_from(members.len()).unwrap_or(u32::MAX);
                if let Some(found) = self.rooms.iter_mut().find(|item| item.name == *room) {
                    found.users = count;
                    found.joined = true;
                } else {
                    self.rooms.push(crate::model::Room {
                        name: room.clone(),
                        users: count,
                        joined: true,
                    });
                }
                self.store_members(room, members.clone());
                self.order_rooms();
                return;
            }
            SessionEvent::RoomListed { room, users } => {
                if let Some(found) = self.rooms.iter_mut().find(|item| item.name == *room) {
                    found.users = *users;
                } else {
                    self.rooms.push(crate::model::Room {
                        name: room.clone(),
                        users: *users,
                        joined: false,
                    });
                }
                self.order_rooms();
                return;
            }
            SessionEvent::Ticker { room, text } => {
                self.tickers.insert(room.clone(), text.clone());
                return;
            }
            SessionEvent::PortMapped { external } => {
                self.notice = format!("port mapped: {external}");
                return;
            }
            SessionEvent::PortMapFailed { message } => {
                self.notice = format!("port map failed: {message}");
                return;
            }
            SessionEvent::QualityHit(hit) => {
                self.note_quality_hit(hit.clone());
                return;
            }
            SessionEvent::SearchBegan(token) => {
                if let Some(retry) = &mut self.album_retry
                    && retry.armed
                {
                    retry.armed = false;
                    retry.token = Some(*token);
                    retry.wait = ALBUM_RETRY_TICKS;
                } else if let Some(retry) = &mut self.file_retry
                    && retry.armed
                {
                    retry.armed = false;
                    retry.token = Some(*token);
                    retry.wait = ALBUM_RETRY_TICKS;
                }
                self.shown_search = ShownSearch::Token(*token);
                self.hits.clear();
                self.open_folders.clear();
                self.note_hits();
                self.note_open();
                self.asked_folders.clear();
                self.album_downloads.clear();
                self.cursors[View::Search.index()] = 0;
                self.set_list_origin(View::Search.index(), 0);
                self.scroll_hold.list[View::Search.index()] = None;
                return;
            }
            SessionEvent::SearchResult(token, hit) => {
                self.note_album_hit(*token, hit);
                self.note_file_hit(*token, hit);
                if !self.keeps_result(*token) {
                    return;
                }
                if !self.is_blocked(&hit.user) {
                    self.hits.push(hit.clone());
                    self.note_hits();
                    if self.feed == Feed::Live {
                        let count = self.hits.len();
                        self.notice = if count == 1 {
                            "1 result".to_owned()
                        } else {
                            format!("{count} results")
                        };
                    }
                }
                return;
            }
            SessionEvent::Folder {
                user,
                directory,
                files,
            } => {
                let directory = directory.replace('/', "\\");
                let key = folder_key(user, &directory);
                if !self.asked_folders.contains(&key) {
                    return;
                }
                self.open_folders.insert(key.clone());
                self.note_open();
                let fetch = self.album_downloads.contains(&key);
                let mut known: HashSet<String> = self
                    .hits
                    .iter()
                    .filter(|hit| hit.user.as_str() == user.as_str())
                    .map(|hit| hit.path.clone())
                    .collect();
                let mut added = 0u32;
                for file in files {
                    if file.user.as_str() != user.as_str() || self.is_blocked(&file.user) {
                        continue;
                    }
                    if parent_dir(&file.path) != directory {
                        continue;
                    }
                    if !known.insert(file.path.clone()) {
                        continue;
                    }
                    if fetch {
                        self.pending_downloads
                            .push(crate::session::DownloadRequest {
                                user: file.user.clone(),
                                path: file.path.clone(),
                                size: file.size,
                                folder: Some(directory.clone()),
                                stage: false,
                            });
                    }
                    self.hits.push(file.clone());
                    added = added.saturating_add(1);
                }
                if added > 0 {
                    self.note_hits();
                    self.notice = format!("{added} files in {directory}");
                }
                return;
            }
            SessionEvent::Network { parent, children } => {
                self.parent.clone_from(parent);
                self.children = *children;
                return;
            }
            SessionEvent::ShareScanFailed { message } => {
                if self
                    .album_retry
                    .as_ref()
                    .is_some_and(|retry| retry.armed && retry.token.is_none())
                {
                    self.album_retry = None;
                    self.notice = format!("album search failed: {message}");
                    self.push_log(message);
                    self.start_next_album_retry();
                    self.start_next_file_retry();
                    return;
                }
                if self
                    .file_retry
                    .as_ref()
                    .is_some_and(|retry| retry.armed && retry.token.is_none())
                {
                    self.file_retry = None;
                    self.notice = format!("file search failed: {message}");
                    self.push_log(message);
                    self.start_next_file_retry();
                    self.start_next_album_retry();
                    return;
                }
                self.notice.clone_from(message);
                self.push_log(message);
                return;
            }
            SessionEvent::ShareRoot { path } => {
                self.remember_public_share(path);
                return;
            }
            SessionEvent::Shares {
                public_files,
                public_folders,
                buddy_files,
                buddy_folders,
                trusted_files,
                trusted_folders,
            } => {
                if self.feed == Feed::Live {
                    self.shares = share_counts(
                        (*public_files, *public_folders),
                        (*buddy_files, *buddy_folders),
                        (*trusted_files, *trusted_folders),
                    );
                }
                return;
            }
            SessionEvent::AlbumFailed { user, album } => {
                self.begin_album_retry(album.clone(), user.clone());
                return;
            }
            SessionEvent::FileUnshared {
                user,
                file,
                path,
                folder,
            } => {
                self.begin_file_retry(file.clone(), user.clone(), path.clone(), folder.clone());
                return;
            }
            SessionEvent::Transfer(transfer) => {
                let upgrade = self.upgrade_transfer(transfer);
                let already_finished = {
                    let rows = if self.feed == Feed::Live {
                        &self.transfers
                    } else {
                        &self.live
                    };
                    rows.iter().any(|item| {
                        item.direction == transfer.direction
                            && item.user == transfer.user
                            && item.path == transfer.path
                            && item.state == TransferState::Finished
                    })
                };
                if upgrade {
                    if transfer.state == TransferState::Finished && !already_finished {
                        self.push_log(format!(
                            "Download finished: user {}, file {}",
                            transfer.user, transfer.path
                        ));
                        self.start_verify(&transfer.user, &transfer.path);
                    } else if upgrade_gave_up(transfer.state) {
                        self.give_up_upgrade(&transfer.user, &transfer.path);
                    }
                } else if transfer.state == TransferState::Finished && !already_finished {
                    let kind = match transfer.direction {
                        Direction::Upload => "Upload",
                        Direction::Download => "Download",
                    };
                    self.push_log(format!(
                        "{kind} finished: user {}, file {}",
                        transfer.user, transfer.path
                    ));
                    if transfer.direction == Direction::Download {
                        self.library_dirty = true;
                        self.start_library_scan();
                    }
                }
                let rows = if self.feed == Feed::Live {
                    &mut self.transfers
                } else {
                    &mut self.live
                };
                if let Some(found) = rows.iter_mut().find(|item| {
                    item.direction == transfer.direction
                        && item.user == transfer.user
                        && item.path == transfer.path
                }) {
                    *found = transfer.clone();
                } else {
                    rows.push(transfer.clone());
                }
                if upgrade
                    && matches!(
                        transfer.state,
                        TransferState::TooManyFiles
                            | TransferState::TooManyMegabytes
                            | TransferState::InternalError
                            | TransferState::LastTryFailed
                    )
                {
                    self.retry_upgrade(&transfer.user, &transfer.path);
                } else if upgrade {
                    self.touch_quality();
                }
                return;
            }
        }
        self.session = Some(event);
    }

    pub fn session(&self) -> Option<&SessionEvent> {
        self.session.as_ref()
    }

    pub fn logged_in(&self) -> bool {
        self.banner.is_some()
    }

    /// Interface address the session is using. Empty until the listen socket is up.
    pub fn listen_ip(&self) -> Option<&str> {
        self.listen_ip.as_deref().filter(|ip| !ip.is_empty())
    }

    /// Header text for the current session.
    pub fn connection_label(&self) -> String {
        if let Some(banner) = &self.banner {
            return format!("{} · {banner}", self.account);
        }
        match &self.session {
            Some(SessionEvent::LoginFailed { reason }) => reason.clone(),
            Some(SessionEvent::TimedOut) => "login timed out".to_owned(),
            Some(SessionEvent::Disconnected { message }) => message.clone(),
            Some(SessionEvent::ListenFailed { message }) => message.clone(),
            Some(SessionEvent::Kicked) => "logged in elsewhere".to_owned(),
            _ if self.signing_in || self.account.is_empty() => "sign in".to_owned(),
            _ => "signing in".to_owned(),
        }
    }

    pub fn down_history(&self) -> &[u64] {
        &self.down
    }

    pub fn up_history(&self) -> &[u64] {
        &self.up
    }

    pub fn cursor(&self) -> usize {
        self.cursors[self.view.index()]
    }

    pub fn artist_names(&self) -> Vec<String> {
        self.catalog
            .artists
            .iter()
            .map(|artist| artist.name.clone())
            .collect()
    }

    pub fn album_names(&self) -> Vec<String> {
        self.selected_artist()
            .map(|artist| {
                artist
                    .albums
                    .iter()
                    .map(|album| album.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn song_labels(&self) -> Vec<String> {
        self.selected_album()
            .map(|album| {
                album
                    .songs
                    .iter()
                    .map(crate::library::Song::label)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn library_column(&self) -> u8 {
        self.library_column
    }

    pub fn library_index(&self, column: u8) -> usize {
        self.library_cursors[usize::from(column.min(2))]
    }

    pub fn spectrum(&self) -> Vec<f32> {
        self.player.spectrum()
    }

    pub fn playback_line(&self) -> String {
        self.player.headline()
    }

    pub fn track_rows(&self) -> Vec<String> {
        let mut rows = self.player.snapshot().rows();
        let idle = rows.first().is_some_and(|row| row == "nothing playing");
        let path = if idle {
            self.selected_song().map(|song| song.path.clone())
        } else {
            let loaded = self.player.loaded_path();
            (!loaded.as_os_str().is_empty()).then_some(loaded)
        };
        if let Some(path) = path
            && let Some(quality) = self.catalog.quality(&path)
        {
            rows.push(quality.row());
        }
        rows
    }

    pub fn transport(&self) -> (String, String, f64) {
        let now = self.player.snapshot();
        (now.position_text(), now.length_text(), now.fraction())
    }

    pub fn transport_button(&self) -> TransportButton {
        let now = self.player.snapshot();
        if now.playing {
            TransportButton::Play
        } else if now.loaded
            && now.position >= std::time::Duration::from_millis(50)
            && now.position + std::time::Duration::from_millis(20) < now.duration
        {
            TransportButton::Pause
        } else {
            TransportButton::Stop
        }
    }

    pub fn seek_column(&mut self, column: u16, origin: u16, width: u16) {
        self.player
            .seek(crate::playback::bar_fraction(column, origin, width));
    }

    fn press_play(&mut self) {
        if self.player.loaded() {
            self.player.resume();
        } else {
            self.play_library();
        }
    }

    pub fn player_ready(&self) -> bool {
        self.player.ready()
    }

    /// Applies a folder listing as soon as one is ready, then the finished scan.
    pub fn poll_library(&mut self) {
        let Some(job) = self.library_job.take() else {
            return;
        };
        if let Ok(mut live) = job.live.lock()
            && let Some((epoch, catalog, removed, moved)) = live.take()
            && epoch == self.library_epoch
            && self.catalog.artists.is_empty()
        {
            self.catalog = catalog;
            self.pending_forget.extend(removed);
            self.pending_moves.extend(moved);
            self.clamp_library();
        }
        if !job.handle.is_finished() {
            self.library_job = Some(job);
            return;
        }
        if let Ok((epoch, catalog, removed, moved)) = job.handle.join()
            && epoch == self.library_epoch
        {
            self.catalog = catalog;
            self.pending_forget.extend(removed);
            self.pending_moves.extend(moved);
            if self.quality_drop_scan {
                self.catalog.forget_quality();
                self.quality_drop_scan = false;
                if let Some(job) = &mut self.quality_job {
                    job.paths.clear();
                    self.quality_drop_job = true;
                }
            }
            self.clamp_library();
            self.save_library();
            self.start_quality_job();
        }
        if self.library_dirty {
            self.start_library_scan();
        }
    }

    /// Collects finished measurements without waiting on the workers.
    pub fn poll_quality(&mut self) {
        self.poll_verify();
        let Some(mut job) = self.quality_job.take() else {
            self.start_quality_job();
            self.consider_upgrade();
            return;
        };
        let mut changed = false;
        while let Ok((path, quality)) = job.rx.try_recv() {
            changed |= self.take_measurement(&mut job, path, quality);
        }
        let done = job.handles.iter().all(|handle| handle.is_finished());
        if done {
            while let Ok((path, quality)) = job.rx.try_recv() {
                changed |= self.take_measurement(&mut job, path, quality);
            }
            for handle in job.handles {
                let _ = handle.join();
            }
            if self.quality_drop_job {
                self.quality_drop_job = false;
                changed = false;
            } else if changed {
                self.save_library();
            }
            self.start_quality_job();
        } else {
            self.quality_job = Some(job);
        }
        if changed {
            self.touch_quality();
        }
        self.consider_upgrade();
    }

    pub fn library_scan_pending(&self) -> bool {
        self.library_job.is_some()
    }

    /// Reloads the library on the library thread. A saved catalog is already
    /// on screen. Folder names fill an empty library before tags are read.
    fn refresh_library(&mut self) {
        self.library_dirty = true;
        self.start_library_scan();
    }

    fn start_library_scan(&mut self) {
        if self.library_job.is_some() {
            return;
        }
        let Some(root) = self.download_root() else {
            self.library_dirty = false;
            self.load_library();
            return;
        };
        self.library_dirty = false;
        let epoch = self.library_epoch;
        let catalog = self.catalog.clone();
        let skip = self.incomplete_skip(&root);
        let live = std::sync::Arc::new(std::sync::Mutex::new(None));
        let publish = std::sync::Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name("library".to_owned())
            .spawn(move || {
                if catalog.artists.is_empty() {
                    let preview = crate::library::Catalog::preview_skipping(&root, skip.as_deref());
                    if let Ok(mut slot) = publish.lock() {
                        *slot = Some((epoch, preview, Vec::new(), Vec::new()));
                    }
                }
                let (catalog, moved) = catalog.reconcile_skipping(&root, skip.as_deref());
                let removed = crate::library::sweep(&root, skip.as_deref());
                (epoch, catalog, removed, moved)
            })
            .ok();
        self.library_job = handle.map(|handle| LibraryJob { handle, live });
        if self.library_job.is_none() {
            self.load_library();
        }
    }

    fn load_library(&mut self) {
        let Some((_, config)) = &self.stored else {
            self.catalog = crate::library::Catalog::default();
            self.clamp_library();
            return;
        };
        if config.download_dir.is_empty() {
            self.catalog = crate::library::Catalog::default();
        } else {
            let root = std::path::PathBuf::from(&config.download_dir);
            let skip = self.incomplete_skip(&root);
            let (catalog, moved) = self.catalog.reconcile_skipping(&root, skip.as_deref());
            self.catalog = catalog;
            self.pending_moves.extend(moved);
            self.pending_forget
                .extend(crate::library::sweep(&root, skip.as_deref()));
        }
        self.clamp_library();
        self.save_library();
    }

    fn save_library(&self) {
        let Some(path) = &self.library_path else {
            return;
        };
        let Some(root) = self.download_root() else {
            return;
        };
        self.catalog.store(path, &root);
    }

    /// Forgets every saved verdict and measures the library again.
    fn recheck_quality(&mut self) {
        self.catalog.forget_quality();
        self.save_library();
        self.touch_quality();
        self.notice = "rechecking quality".to_owned();
        if self.library_job.is_some() {
            self.quality_drop_scan = true;
        }
        if let Some(job) = &mut self.quality_job {
            job.paths.clear();
            self.quality_drop_job = true;
        } else {
            self.start_quality_job();
        }
    }

    fn take_measurement(
        &mut self,
        job: &mut QualityJob,
        path: PathBuf,
        quality: crate::quality::Quality,
    ) -> bool {
        job.paths.retain(|pending| pending != &path);
        if self.quality_drop_job {
            return false;
        }
        self.catalog.set_quality(&path, quality)
    }

    fn start_quality_job(&mut self) {
        if self.quality_job.is_some() {
            return;
        }
        let playing = if self.player.is_playing() {
            self.player.loaded_path()
        } else {
            PathBuf::new()
        };
        let pending = self.catalog.pending_analysis(&playing);
        if pending.is_empty() {
            return;
        }
        let paths = pending.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let mid = pending.len().div_ceil(2);
        let mut handles = Vec::new();
        for chunk in [pending[..mid].to_vec(), pending[mid..].to_vec()] {
            if chunk.is_empty() {
                continue;
            }
            let tx = tx.clone();
            let Ok(handle) = std::thread::Builder::new()
                .name("quality".to_owned())
                .spawn(move || {
                    for path in chunk {
                        let quality = crate::quality::analyze_path(&path);
                        let _ = tx.send((path, quality));
                        std::thread::yield_now();
                    }
                })
            else {
                continue;
            };
            handles.push(handle);
        }
        drop(tx);
        if handles.is_empty() {
            return;
        }
        self.quality_job = Some(QualityJob { rx, handles, paths });
    }

    fn consider_upgrade(&mut self) {
        if !self.logged_in() || self.quality_probe.is_some() || self.pending_probe.is_some() {
            return;
        }
        loop {
            let Some((song, quality, goal)) = self.next_upgrade() else {
                return;
            };
            let query =
                crate::protocol::sanitize_search_query(&format!("{} {}", song.artist, song.title));
            if query.is_empty() {
                self.catalog.mark_upgrade_attempted(&song.path);
                self.save_library();
                continue;
            }
            self.quality_probe = Some(QualityProbe {
                path: song.path,
                artist: song.artist,
                title: song.title,
                duration: quality.duration_secs,
                goal,
                hits: Vec::new(),
                ticks: 0,
            });
            self.pending_probe = Some(query);
            self.touch_quality();
            return;
        }
    }

    fn next_upgrade(
        &self,
    ) -> Option<(
        crate::library::Song,
        crate::quality::Quality,
        crate::quality::UpgradeGoal,
    )> {
        self.catalog.entries().find_map(|(song, quality)| {
            let quality = quality.cloned()?;
            let goal = quality.goal()?;
            if self.upgrade_claimed(&song.path) {
                return None;
            }
            Some((song.clone(), quality, goal))
        })
    }

    fn upgrade_claimed(&self, path: &Path) -> bool {
        self.quality_probe
            .as_ref()
            .is_some_and(|probe| probe.path == path)
            || self
                .quality_inflight
                .iter()
                .any(|upgrade| upgrade.original == path)
            || self.quality_held.iter().any(|held| held.path == path)
    }

    fn busy_users(&self) -> Vec<String> {
        self.quality_inflight
            .iter()
            .filter(|upgrade| !upgrade.checking)
            .map(|upgrade| upgrade.user.clone())
            .collect()
    }

    fn advance_quality_probe(&mut self) {
        let ready = if let Some(probe) = &mut self.quality_probe {
            probe.ticks = probe.ticks.saturating_add(1);
            probe.ticks >= 40 || probe.hits.len() >= crate::quality::PROBE_HITS
        } else {
            false
        };
        if ready {
            self.finish_probe();
        }
    }

    fn finish_probe(&mut self) {
        let Some(probe) = self.quality_probe.take() else {
            return;
        };
        self.pending_probe = None;
        self.offer_upgrade(
            probe.path,
            probe.artist,
            probe.title,
            probe.duration,
            probe.goal,
            probe.hits,
        );
        self.consider_upgrade();
    }

    /// Downloads the best remaining copy. A user who already refused because their
    /// queue was full is skipped. Busy users wait. Nobody left ends the attempt.
    fn offer_upgrade(
        &mut self,
        original: PathBuf,
        artist: String,
        title: String,
        duration: Option<u32>,
        goal: crate::quality::UpgradeGoal,
        hits: Vec<SearchHit>,
    ) {
        let skipped = self.skipped_users(&original);
        let skipped_ref: Vec<&str> = skipped.iter().map(String::as_str).collect();
        let another = crate::quality::pick_upgrade_except(
            &hits,
            goal,
            &artist,
            &title,
            duration,
            &skipped_ref,
        );
        let blocked = self.blocked_users(&original);
        let blocked_ref: Vec<&str> = blocked.iter().map(String::as_str).collect();
        if let Some(hit) = crate::quality::pick_upgrade_except(
            &hits,
            goal,
            &artist,
            &title,
            duration,
            &blocked_ref,
        ) {
            let user = hit.user.clone();
            let path = hit.path.clone();
            let size = hit.size;
            self.queue_upgrade(
                InflightUpgrade {
                    original,
                    artist,
                    title,
                    duration,
                    goal,
                    user,
                    virtual_path: path,
                    hits,
                    checking: false,
                },
                size,
            );
            return;
        }
        if another.is_some() {
            self.quality_held.push(HeldUpgrade {
                path: original,
                artist,
                title,
                duration,
                goal,
                hits,
            });
            self.touch_quality();
            return;
        }
        self.catalog.mark_upgrade_attempted(&original);
        self.save_library();
        self.clear_skipped(&original);
        self.touch_quality();
    }

    fn queue_upgrade(&mut self, upgrade: InflightUpgrade, size: u64) {
        self.pending_downloads
            .push(crate::session::DownloadRequest {
                user: upgrade.user.clone(),
                path: upgrade.virtual_path.clone(),
                size,
                folder: None,
                stage: true,
            });
        self.quality_inflight.push(upgrade);
        self.touch_quality();
    }

    fn retry_upgrade(&mut self, user: &str, virtual_path: &str) {
        let Some(index) = self.quality_inflight.iter().position(|upgrade| {
            !upgrade.checking && upgrade.user == user && upgrade.virtual_path == virtual_path
        }) else {
            return;
        };
        let upgrade = self.quality_inflight.remove(index);
        self.skip_user(&upgrade.original, user);
        self.drop_unshared(user, virtual_path);
        self.offer_upgrade(
            upgrade.original,
            upgrade.artist,
            upgrade.title,
            upgrade.duration,
            upgrade.goal,
            upgrade.hits,
        );
        self.consider_upgrade();
    }

    fn skip_user(&mut self, path: &Path, user: &str) {
        let users = self.quality_skipped.entry(path.to_path_buf()).or_default();
        if !users.iter().any(|name| name == user) {
            users.push(user.to_owned());
        }
    }

    fn skipped_users(&self, path: &Path) -> Vec<String> {
        self.quality_skipped.get(path).cloned().unwrap_or_default()
    }

    fn clear_skipped(&mut self, path: &Path) {
        self.quality_skipped.remove(path);
    }

    fn blocked_users(&self, path: &Path) -> Vec<String> {
        let mut users = self.busy_users();
        for name in self.skipped_users(path) {
            if !users.iter().any(|have| have == &name) {
                users.push(name);
            }
        }
        users
    }

    fn give_up_upgrade(&mut self, user: &str, virtual_path: &str) {
        let Some(index) = self.quality_inflight.iter().position(|upgrade| {
            !upgrade.checking && upgrade.user == user && upgrade.virtual_path == virtual_path
        }) else {
            return;
        };
        let original = self.quality_inflight.remove(index).original;
        self.catalog.mark_upgrade_attempted(&original);
        self.save_library();
        self.clear_skipped(&original);
        self.start_held();
        self.consider_upgrade();
        self.touch_quality();
    }

    fn start_held(&mut self) {
        let held = std::mem::take(&mut self.quality_held);
        for item in held {
            self.offer_upgrade(
                item.path,
                item.artist,
                item.title,
                item.duration,
                item.goal,
                item.hits,
            );
        }
    }

    fn note_quality_hit(&mut self, hit: SearchHit) {
        let ready = if let Some(probe) = &mut self.quality_probe {
            if probe.hits.len() < crate::quality::PROBE_HITS {
                probe.hits.push(hit);
            }
            probe.hits.len() >= crate::quality::PROBE_HITS
        } else {
            false
        };
        self.touch_quality();
        if ready {
            self.finish_probe();
        }
    }

    fn upgrade_transfer(&self, transfer: &Transfer) -> bool {
        self.quality_inflight.iter().any(|upgrade| {
            !upgrade.checking
                && transfer.direction == Direction::Download
                && transfer.user == upgrade.user
                && transfer.path == upgrade.virtual_path
        })
    }

    fn start_verify(&mut self, user: &str, virtual_path: &str) {
        let Some(index) = self.quality_inflight.iter().position(|upgrade| {
            !upgrade.checking && upgrade.user == user && upgrade.virtual_path == virtual_path
        }) else {
            return;
        };
        let upgrade = self.quality_inflight[index].clone();
        let Some(root) = self.download_root() else {
            return;
        };
        let Some(incomplete) = self.incomplete_dir() else {
            self.quality_inflight.remove(index);
            self.catalog.mark_upgrade_attempted(&upgrade.original);
            self.save_library();
            self.start_held();
            self.consider_upgrade();
            self.touch_quality();
            return;
        };
        let Some(original) = self.catalog.quality(&upgrade.original).cloned() else {
            self.quality_inflight.remove(index);
            self.start_held();
            self.consider_upgrade();
            self.touch_quality();
            return;
        };
        let staged = crate::session::quality_stage(&incomplete, &upgrade.virtual_path);
        let original_path = upgrade.original.clone();
        let Ok(handle) = std::thread::Builder::new()
            .name("quality".to_owned())
            .spawn(move || {
                if crate::quality::replace_with(&root, &original_path, &original, &staged).is_some()
                {
                    VerifyDone::Replaced {
                        original: original_path,
                    }
                } else {
                    VerifyDone::Kept {
                        original: original_path,
                    }
                }
            })
        else {
            return;
        };
        self.quality_inflight[index].checking = true;
        self.quality_verify.push(handle);
        self.start_held();
        self.consider_upgrade();
        self.touch_quality();
    }

    fn poll_verify(&mut self) {
        if self.quality_verify.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.quality_verify);
        let mut still = Vec::new();
        let mut changed = false;
        for handle in pending {
            if !handle.is_finished() {
                still.push(handle);
                continue;
            }
            changed = true;
            let Ok(done) = handle.join() else {
                continue;
            };
            let original = match &done {
                VerifyDone::Replaced { original } | VerifyDone::Kept { original } => {
                    original.clone()
                }
            };
            self.quality_inflight
                .retain(|upgrade| upgrade.original != original);
            self.clear_skipped(&original);
            match done {
                VerifyDone::Replaced { original } => {
                    self.catalog.forget(std::slice::from_ref(&original));
                    self.pending_forget.push(original);
                    self.library_dirty = true;
                    self.start_library_scan();
                }
                VerifyDone::Kept { original } => {
                    self.catalog.mark_upgrade_attempted(&original);
                    self.save_library();
                }
            }
        }
        self.quality_verify = still;
        if changed {
            self.touch_quality();
        }
    }

    fn incomplete_dir(&self) -> Option<PathBuf> {
        let (_, config) = self.stored.as_ref()?;
        if config.incomplete_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&config.incomplete_dir))
        }
    }

    pub fn take_quality_probe(&mut self) -> Option<String> {
        self.pending_probe.take()
    }

    fn nudge_library(&mut self, down: bool) {
        let column = usize::from(self.library_column.min(2));
        let len = self.column_len(self.library_column);
        if len == 0 {
            return;
        }
        let cursor = &mut self.library_cursors[column];
        if down {
            *cursor = cursor.saturating_add(1).min(len - 1);
        } else {
            *cursor = cursor.saturating_sub(1);
        }
        self.clamp_library();
    }

    fn shift_library(&mut self, next: bool) {
        self.library_column = if next {
            (self.library_column + 1) % 3
        } else {
            (self.library_column + 2) % 3
        };
    }

    fn nudge_quality(&mut self, down: bool) {
        let column = Self::quality_slot(self.quality_column);
        let len = self.quality_len(self.quality_column);
        if len == 0 {
            return;
        }
        let cursor = &mut self.quality_cursors[column];
        if down {
            *cursor = cursor.saturating_add(1).min(len - 1);
        } else {
            *cursor = cursor.saturating_sub(1);
        }
    }

    fn shift_quality(&mut self, _next: bool) {
        self.quality_column = (self.quality_column + 1) % 2;
    }

    pub fn quality_len(&self, column: u8) -> usize {
        self.ensure_quality_board();
        self.quality_board.borrow().column(column).len()
    }

    pub fn quality_label(&self, column: u8, index: usize) -> String {
        self.ensure_quality_board();
        self.quality_board.borrow().column(column)[index]
            .label
            .clone()
    }

    /// The lines above the columns, for the highlighted row.
    pub fn quality_detail(&self) -> Vec<String> {
        self.ensure_quality_board();
        let board = self.quality_board.borrow();
        let mut lines = vec![format!(
            "measuring {}   replacing {}",
            board.measuring.len(),
            board.replacing.len()
        )];
        let column = board.column(self.quality_column);
        if let Some(line) = column.get(self.quality_index(self.quality_column)) {
            lines.extend(line.detail.iter().cloned());
        } else {
            lines.push("idle".to_owned());
        }
        lines
    }

    fn touch_quality(&self) {
        self.quality_dirty.set(true);
    }

    fn ensure_quality_board(&self) {
        if !self.quality_dirty.get() {
            return;
        }
        let board = self.build_quality_board();
        *self.quality_board.borrow_mut() = board;
        self.quality_dirty.set(false);
    }

    fn build_quality_board(&self) -> QualityBoard {
        let mut songs = HashMap::new();
        for (song, quality) in self.catalog.entries() {
            songs.insert(song.path.as_path(), (song, quality));
        }
        QualityBoard {
            measuring: self.measuring_lines(),
            replacing: self.replacing_lines(&songs),
        }
    }

    fn measuring_lines(&self) -> Vec<QualityLine> {
        let playing = if self.player.is_playing() {
            self.player.loaded_path()
        } else {
            PathBuf::new()
        };
        let active = self
            .quality_job
            .as_ref()
            .map(|job| {
                job.paths
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let mut lines = Vec::new();
        for (song, quality) in self.catalog.entries() {
            if quality.is_some() {
                continue;
            }
            let state = if song.path == playing {
                "playing"
            } else if active.contains(song.path.as_path()) {
                "measuring"
            } else {
                "waiting"
            };
            lines.push(QualityLine {
                label: format!("{}  {state}", song.title),
                detail: vec![
                    state.to_owned(),
                    song.title.clone(),
                    format!("{} — {}", song.artist, song.album),
                    "not measured yet".to_owned(),
                ],
            });
        }
        lines
    }

    fn replacing_lines(
        &self,
        songs: &HashMap<&Path, (&crate::library::Song, Option<&crate::quality::Quality>)>,
    ) -> Vec<QualityLine> {
        let mut lines = Vec::new();
        let mut claimed = Vec::new();
        for upgrade in &self.quality_inflight {
            claimed.push(upgrade.original.clone());
            lines.push(self.inflight_line(songs, upgrade));
        }
        if let Some(probe) = &self.quality_probe
            && !claimed.iter().any(|path| path == &probe.path)
        {
            claimed.push(probe.path.clone());
            lines.push(self.probe_line(songs, probe));
        }
        for held in &self.quality_held {
            if claimed.iter().any(|path| path == &held.path) {
                continue;
            }
            claimed.push(held.path.clone());
            lines.push(self.held_line(songs, held));
        }
        for (song, quality) in self.catalog.entries() {
            let Some(quality) = quality else {
                continue;
            };
            if quality.goal().is_none() {
                continue;
            }
            if claimed.iter().any(|path| path == &song.path) {
                continue;
            }
            lines.push(self.need_line(song, quality, "waiting its turn"));
        }
        lines
    }

    fn probe_line(
        &self,
        songs: &HashMap<&Path, (&crate::library::Song, Option<&crate::quality::Quality>)>,
        probe: &QualityProbe,
    ) -> QualityLine {
        if let Some((song, Some(quality))) = songs.get(probe.path.as_path()).copied() {
            return self.need_line(song, quality, "looking now");
        }
        let need = goal_phrase(probe.goal);
        QualityLine {
            label: format!("{}  needs {need}", probe.title),
            detail: vec![
                probe.title.clone(),
                format!("needs {need}"),
                "looking now".to_owned(),
            ],
        }
    }

    fn held_line(
        &self,
        songs: &HashMap<&Path, (&crate::library::Song, Option<&crate::quality::Quality>)>,
        held: &HeldUpgrade,
    ) -> QualityLine {
        if let Some((song, Some(quality))) = songs.get(held.path.as_path()).copied() {
            return self.need_line(song, quality, "waiting for a free user");
        }
        let need = goal_phrase(held.goal);
        QualityLine {
            label: format!("{}  needs {need}", held.title),
            detail: vec![
                held.title.clone(),
                format!("needs {need}"),
                "waiting for a free user".to_owned(),
            ],
        }
    }

    fn need_line(
        &self,
        song: &crate::library::Song,
        quality: &crate::quality::Quality,
        note: &str,
    ) -> QualityLine {
        let need = quality.goal().map(goal_phrase).unwrap_or("a lossless copy");
        let mut detail = vec![
            song.title.clone(),
            format!("{} — {}", song.artist, song.album),
            format!("have {}", quality.facts()),
            format!("needs {need}"),
        ];
        if !self.logged_in() {
            detail.push("signed out".to_owned());
        } else {
            detail.push(note.to_owned());
        }
        QualityLine {
            label: format!("{}  needs {need}", song.title),
            detail,
        }
    }

    fn inflight_line(
        &self,
        songs: &HashMap<&Path, (&crate::library::Song, Option<&crate::quality::Quality>)>,
        upgrade: &InflightUpgrade,
    ) -> QualityLine {
        let known = songs.get(upgrade.original.as_path()).copied();
        let title = known
            .map(|(song, _)| song.title.clone())
            .unwrap_or_else(|| {
                upgrade
                    .original
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("song")
                    .to_owned()
            });
        let action = self.replacement_action(upgrade);
        let offered = chosen_advertised(upgrade);
        let label = match &offered {
            Some(quality) => format!("{title}  {quality} · {action}"),
            None => format!("{title}  {action}"),
        };
        let mut detail = vec![title.clone()];
        if let Some((song, quality)) = known {
            detail.push(format!("{} — {}", song.artist, song.album));
            if let Some(quality) = quality {
                detail.push(format!("have {}", quality.facts()));
            }
        }
        detail.push(match &offered {
            Some(quality) => format!("{quality} · {action}"),
            None => action,
        });
        detail.push(format!(
            "from {} · {}",
            upgrade.user,
            remote_file_name(&upgrade.virtual_path)
        ));
        QualityLine { label, detail }
    }

    fn replacement_action(&self, upgrade: &InflightUpgrade) -> String {
        if upgrade.checking {
            return "checking the new file".to_owned();
        }
        let Some(transfer) = self.transfers.iter().find(|transfer| {
            transfer.direction == Direction::Download
                && transfer.user == upgrade.user
                && transfer.path == upgrade.virtual_path
        }) else {
            return "starting the download".to_owned();
        };
        match transfer.state {
            TransferState::Transferring => format!("downloading {}%", transfer.percent()),
            TransferState::Queued => "in queue".to_owned(),
            TransferState::Finished => "checking the new file".to_owned(),
            _ => transfer.status(),
        }
    }
}

fn goal_phrase(goal: crate::quality::UpgradeGoal) -> &'static str {
    match goal {
        crate::quality::UpgradeGoal::TwentyFour => "a 24-bit copy",
        crate::quality::UpgradeGoal::AnyLossless => "a lossless copy",
    }
}

fn chosen_advertised(upgrade: &InflightUpgrade) -> Option<String> {
    let hit = upgrade
        .hits
        .iter()
        .find(|hit| hit.user == upgrade.user && hit.path == upgrade.virtual_path)?;
    let depth = hit
        .bit_depth
        .filter(|bits| *bits > 0)
        .map(crate::quality::depth_label);
    let rate = hit
        .sample_rate
        .filter(|rate| *rate > 0)
        .map(crate::quality::rate_label);
    match (depth, rate) {
        (Some(depth), Some(rate)) => Some(format!("{depth} · {rate}")),
        (Some(depth), None) => Some(depth),
        (None, Some(rate)) => Some(rate),
        (None, None) => None,
    }
}

fn remote_file_name(path: &str) -> &str {
    path.rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

impl App {
    fn ask_delete(&mut self) {
        let Some((heading, detail, paths)) = self.deletion() else {
            return;
        };
        let Some(root) = self.download_root() else {
            return;
        };
        let paths: Vec<_> = paths
            .into_iter()
            .filter(|path| path.starts_with(&root) && path != &root)
            .collect();
        if paths.is_empty() {
            return;
        }
        self.confirm = Some(DeletePrompt {
            heading,
            detail,
            paths,
        });
    }

    fn deletion(&self) -> Option<(String, String, Vec<std::path::PathBuf>)> {
        match self.library_column {
            0 => {
                let artist = self.selected_artist()?;
                let paths = artist
                    .albums
                    .iter()
                    .flat_map(|album| album.songs.iter().map(|song| song.path.clone()))
                    .collect::<Vec<_>>();
                let count = paths.len();
                Some((
                    format!("Remove {} from disk?", artist.name),
                    file_count(count),
                    paths,
                ))
            }
            1 => {
                let album = self.selected_album()?;
                let paths = album
                    .songs
                    .iter()
                    .map(|song| song.path.clone())
                    .collect::<Vec<_>>();
                let count = paths.len();
                Some((
                    format!("Remove {} from disk?", album.name),
                    file_count(count),
                    paths,
                ))
            }
            _ => {
                let song = self.selected_song()?;
                Some((
                    format!("Remove {} from disk?", song.title),
                    "This file will be deleted.".to_owned(),
                    vec![song.path.clone()],
                ))
            }
        }
    }

    fn commit_delete(&mut self) {
        let Some(prompt) = self.confirm.take() else {
            return;
        };
        let Some(root) = self.download_root() else {
            return;
        };
        self.player.drop_if(&prompt.paths);
        crate::library::discard(&root, &prompt.paths);
        self.catalog.forget(&prompt.paths);
        self.clamp_library();
        self.pending_forget.extend(prompt.paths);
        self.library_epoch = self.library_epoch.wrapping_add(1);
        self.library_dirty = true;
        if self.library_job.is_none() {
            self.refresh_library();
        }
    }

    /// The incomplete folder when it lives inside the music folder.
    fn incomplete_skip(&self, root: &std::path::Path) -> Option<std::path::PathBuf> {
        let (_, config) = self.stored.as_ref()?;
        if config.incomplete_dir.is_empty() {
            return None;
        }
        let path = std::path::PathBuf::from(&config.incomplete_dir);
        (path.starts_with(root) && path != root).then_some(path)
    }

    fn download_root(&self) -> Option<std::path::PathBuf> {
        let (_, config) = self.stored.as_ref()?;
        if config.download_dir.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&config.download_dir))
        }
    }

    fn play_library(&mut self) {
        let Some(song) = self.selected_song().cloned() else {
            self.notice = "nothing to play".to_owned();
            return;
        };
        self.player.play(&song);
        self.notice.clear();
    }

    fn clamp_library(&mut self) {
        for column in 0..3 {
            let len = self.column_len(column);
            if len == 0 {
                self.library_cursors[usize::from(column)] = 0;
            } else {
                self.library_cursors[usize::from(column)] =
                    self.library_cursors[usize::from(column)].min(len - 1);
            }
        }
    }

    fn column_len(&self, column: u8) -> usize {
        match column {
            0 => self.catalog.artists.len(),
            1 => self
                .selected_artist()
                .map(|artist| artist.albums.len())
                .unwrap_or(0),
            _ => self
                .selected_album()
                .map(|album| album.songs.len())
                .unwrap_or(0),
        }
    }

    fn selected_artist(&self) -> Option<&crate::library::Artist> {
        self.catalog.artists.get(self.library_cursors[0])
    }

    fn selected_album(&self) -> Option<&crate::library::Album> {
        self.selected_artist()?.albums.get(self.library_cursors[1])
    }

    fn selected_song(&self) -> Option<&crate::library::Song> {
        self.selected_album()?.songs.get(self.library_cursors[2])
    }

    pub fn transfers(&self) -> &[Transfer] {
        &self.transfers
    }

    pub fn visible_hits(&self) -> Vec<&SearchHit> {
        let needle = if self.feed == Feed::Live {
            String::new()
        } else {
            self.query.trim().to_ascii_lowercase()
        };
        let excluded: Vec<String> = self
            .exclude
            .split_whitespace()
            .map(|token| token.to_ascii_lowercase())
            .filter(|token| !token.is_empty())
            .collect();
        let included: Vec<String> = self
            .include
            .split_whitespace()
            .map(|token| token.to_ascii_lowercase())
            .filter(|token| !token.is_empty())
            .collect();
        let country = self.country_filter.trim().to_ascii_lowercase();
        let need_text = !needle.is_empty() || !excluded.is_empty() || !included.is_empty();
        let mut hits: Vec<&SearchHit> = self
            .hits
            .iter()
            .filter(|hit| {
                let text_ok = if need_text {
                    let text = format!("{} {}", hit.user, hit.path).to_ascii_lowercase();
                    (needle.is_empty() || text.contains(&needle))
                        && excluded.iter().all(|token| !text.contains(token))
                        && included.iter().all(|token| text.contains(token))
                } else {
                    true
                };
                text_ok
                    && self.passes_quality(hit)
                    && (country.is_empty() || hit.country.to_ascii_lowercase().contains(&country))
            })
            .collect();
        hits.sort_by_key(|hit| hit.format_rank());
        hits
    }

    /// Folder rows collapse every file that shares a directory. A lone file stays one row.
    /// The grouped rows stay cached until the hits, filters, or open albums change.
    pub fn search_lines(&self) -> Ref<'_, Vec<SearchLine>> {
        let stamp = self.search_stamp();
        let fresh = self
            .search_cache
            .borrow()
            .as_ref()
            .is_some_and(|(cached, _)| cached == &stamp);
        if !fresh {
            let lines = self.build_search_lines();
            *self.search_cache.borrow_mut() = Some((stamp, lines));
        }
        Ref::map(self.search_cache.borrow(), |cache| {
            &cache.as_ref().expect("search lines").1
        })
    }

    fn search_stamp(&self) -> SearchStamp {
        SearchStamp {
            hits: self.hits_epoch,
            opens: self.open_epoch,
            live: self.feed == Feed::Live,
            query: self.query.clone(),
            exclude: self.exclude.clone(),
            include: self.include.clone(),
            country: self.country_filter.clone(),
            quality: self.quality,
            min_bitrate: self.min_bitrate,
            min_duration: self.min_duration,
            min_size: self.min_size,
            free_only: self.free_only,
            file_type: self.file_type,
        }
    }

    fn note_hits(&mut self) {
        self.hits_epoch = self.hits_epoch.wrapping_add(1);
    }

    fn note_open(&mut self) {
        self.open_epoch = self.open_epoch.wrapping_add(1);
    }

    fn build_search_lines(&self) -> Vec<SearchLine> {
        let hits = self.visible_hits();
        let mut order = Vec::new();
        let mut groups: HashMap<(String, String), Vec<&SearchHit>> = HashMap::new();
        for hit in hits {
            let folder = parent_dir(&hit.path);
            let key = if folder.is_empty() {
                (hit.user.clone(), format!("\0{}", hit.path))
            } else {
                (hit.user.clone(), folder.to_owned())
            };
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(hit);
        }
        order.sort_by_key(|key| {
            let Some(files) = groups.get(key) else {
                return (true, u32::MAX, 1u8, 1u8);
            };
            let album = !key.1.starts_with('\0') && files.len() >= 2;
            row_rank(album, files)
        });
        let mut lines = Vec::new();
        for key in order {
            let Some(files) = groups.get(&key) else {
                continue;
            };
            let album = !key.1.starts_with('\0') && files.len() >= 2;
            if !album {
                lines.push(file_line(files[0], false));
                continue;
            }
            let expanded = self.open_folders.contains(&folder_key(&key.0, &key.1));
            lines.push(folder_line(&key.0, &key.1, files, expanded));
            if expanded {
                for file in files {
                    lines.push(file_line(file, true));
                }
            }
        }
        lines
    }

    pub fn take_folder(&mut self) -> Option<(String, String)> {
        if self.pending_folders.is_empty() {
            None
        } else {
            Some(self.pending_folders.remove(0))
        }
    }

    /// What `d` will download for the highlighted search row.
    pub fn download_cue(&self) -> &'static str {
        if self.selected_is_folder() {
            "d downloads this album"
        } else if self.search_lines().get(self.cursor()).is_some() {
            "d downloads this song"
        } else {
            "d downloads the highlighted row"
        }
    }

    pub fn filter_field(&self) -> Option<u8> {
        self.filter_field
    }

    pub fn filter_lines(&self) -> Vec<String> {
        let bitrate = bitrate_name(&self.min_bitrate);
        let duration = duration_name(&self.min_duration);
        let size = size_name(&self.min_size);
        vec![
            filter_row("quality", quality_name(self.quality)),
            filter_row("bitrate", &bitrate),
            filter_row("duration", &duration),
            filter_row("free slot", if self.free_only { "yes" } else { "any" }),
            filter_row("file type", FILE_TYPES[usize::from(self.file_type)]),
            filter_row("size", &size),
            filter_row("country", blank(&self.country_filter)),
            filter_row("include", blank(&self.include)),
            filter_row("exclude", blank(&self.exclude)),
        ]
    }

    pub fn cycle_filter(&mut self, index: u8) {
        self.filter_field = None;
        match index {
            0 => self.quality = (self.quality + 1) % 3,
            1 => self.min_bitrate = next_step(self.min_bitrate, &BITRATE_STEPS),
            2 => self.min_duration = next_step(self.min_duration, &DURATION_STEPS),
            3 => self.free_only = !self.free_only,
            4 => {
                self.file_type = (self.file_type + 1) % u8::try_from(FILE_TYPES.len()).unwrap_or(1);
            }
            5 => self.min_size = next_step(self.min_size, &SIZE_STEPS),
            6..=8 => self.filter_field = Some(index),
            _ => {}
        }
    }

    fn passes_quality(&self, hit: &SearchHit) -> bool {
        let quality_ok = match self.quality {
            1 => hit.format_rank() == 0,
            2 => hit.format_rank() == 2,
            _ => true,
        };
        let bitrate_ok = self.min_bitrate == 0
            || hit.format_rank() == 0
            || hit.bitrate.is_some_and(|rate| rate >= self.min_bitrate);
        let size_ok = self.min_size == 0 || hit.size >= self.min_size;
        let duration_ok = self.min_duration == 0
            || hit
                .duration
                .is_some_and(|seconds| seconds >= self.min_duration);
        let slot_ok = !self.free_only || hit.free_slot;
        let type_ok =
            self.file_type == 0 || matches_type(hit, FILE_TYPES[usize::from(self.file_type)]);
        quality_ok && bitrate_ok && size_ok && duration_ok && slot_ok && type_ok
    }

    pub fn exclude(&self) -> &str {
        &self.exclude
    }

    pub fn set_exclude(&mut self, exclude: impl Into<String>) {
        self.exclude = exclude.into();
    }

    pub fn set_search_room(&mut self, room: impl Into<String>) {
        self.room = room.into();
    }

    pub fn set_search_user(&mut self, user: impl Into<String>) {
        self.user_target = user.into();
    }

    pub fn take_search(&mut self) -> Option<crate::session::SearchRequest> {
        self.pending.take()
    }

    pub fn take_browse(&mut self) -> Option<String> {
        self.pending_browse.take()
    }

    pub fn take_lists(&mut self) -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
        self.pending_lists.take()
    }

    /// Detail pane for the selected user, including a peer's user-info reply when one arrived.
    pub fn user_detail(&self) -> String {
        let Some(user) = self.users.get(self.cursor()) else {
            return "no user selected".to_owned();
        };
        let mut body = format!(
            "user        {}\nstatus      {}\ncountry     {}\nlast seen   {}\nnote        {}\nnotify      {}\npriority    {}\ntrusted     {}",
            user.name,
            user.status.label(),
            user.country,
            user.last_seen,
            user.note,
            on_off(user.notify),
            on_off(user.prioritized),
            on_off(user.trusted),
        );
        if let Some(info) = self.user_info.get(&user.name) {
            body.push_str(&format!(
                "\n\n{}\nuploads     {}\nqueue       {}\nfree slot   {}",
                info.description,
                info.total_uploads,
                info.queue_size,
                on_off(info.slots_available),
            ));
        }
        if let Some((likes, hates)) = self.interests.get(&user.name) {
            if !likes.is_empty() {
                body.push_str("\nlikes       ");
                body.push_str(&likes.join(", "));
            }
            if !hates.is_empty() {
                body.push_str("\nhates       ");
                body.push_str(&hates.join(", "));
            }
        }
        if let Some((files, dirs)) = self.file_stats.get(&user.name) {
            body.push_str(&format!("\nfiles       {files}\ndirs        {dirs}"));
        }
        body
    }

    pub fn take_unavailable(&mut self) -> Option<(String, String)> {
        if self.pending_unavailable.is_empty() {
            None
        } else {
            Some(self.pending_unavailable.remove(0))
        }
    }

    pub fn take_file_unavailable(&mut self) -> Option<(String, String)> {
        if self.pending_file_unavailable.is_empty() {
            None
        } else {
            Some(self.pending_file_unavailable.remove(0))
        }
    }

    pub fn take_download(&mut self) -> Option<crate::session::DownloadRequest> {
        if self.pending_downloads.is_empty() {
            None
        } else {
            Some(self.pending_downloads.remove(0))
        }
    }

    pub fn take_cancel(&mut self) -> Option<(Direction, String, String)> {
        if self.pending_cancel.is_empty() {
            None
        } else {
            Some(self.pending_cancel.remove(0))
        }
    }

    /// `Some((downloads, uploads))` when `X` should drop finished rows of those lists.
    pub fn take_clear_finished(&mut self) -> Option<(bool, bool)> {
        self.pending_clear_finished.take()
    }

    pub fn feed(&self) -> Feed {
        self.feed
    }

    pub fn live_transfers(&self) -> &[Transfer] {
        if self.feed == Feed::Live {
            &[]
        } else {
            &self.live
        }
    }

    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    pub fn child_count(&self) -> u32 {
        self.children
    }

    /// Uploads currently transferring, over the configured upload slots.
    pub fn upload_slot_line(&self) -> String {
        let slots = self
            .stored
            .as_ref()
            .map(|(_, config)| config.upload_slots)
            .unwrap_or_else(|| Config::default().upload_slots);
        let used = |rows: &[Transfer]| {
            rows.iter()
                .filter(|row| {
                    row.direction == Direction::Upload && row.state == TransferState::Transferring
                })
                .count()
        };
        let mut busy = used(&self.transfers);
        if self.feed != Feed::Live {
            busy += used(&self.live);
        }
        format!("{busy}/{slots}")
    }

    pub fn browse(&self) -> &[BrowseRow] {
        &self.browse
    }

    pub fn rooms(&self) -> &[Room] {
        &self.rooms
    }

    /// Alphabetical by name. A focused highlight stays on that person.
    fn store_members(&mut self, room: &str, mut members: Vec<RoomPerson>) {
        members.sort_by(|left, right| left.name.cmp(&right.name));
        let slot = View::Chat.index();
        let watching = self
            .rooms
            .get(self.cursors[slot])
            .is_some_and(|item| item.name == room);
        let selected = if watching && self.member_focus {
            self.members
                .get(room)
                .and_then(|list| list.get(self.member_cursor))
                .map(|person| person.name.clone())
        } else {
            None
        };
        self.members.insert(room.to_owned(), members);
        if !watching {
            return;
        }
        let len = self.members.get(room).map(Vec::len).unwrap_or(0);
        if let Some(name) = selected
            && let Some(index) = self
                .members
                .get(room)
                .and_then(|list| list.iter().position(|person| person.name == name))
        {
            self.member_cursor = index;
            return;
        }
        if len == 0 {
            self.member_cursor = 0;
        } else if self.member_cursor >= len {
            self.member_cursor = len - 1;
        }
    }

    /// Largest participant count first. Equal counts are alphabetical by name.
    /// The highlighted room stays highlighted when the rows move.
    fn order_rooms(&mut self) {
        let slot = View::Chat.index();
        let selected = self
            .rooms
            .get(self.cursors[slot])
            .map(|room| room.name.clone());
        self.rooms.sort_by(|left, right| {
            right
                .users
                .cmp(&left.users)
                .then_with(|| left.name.cmp(&right.name))
        });
        if let Some(name) = selected
            && let Some(index) = self.rooms.iter().position(|room| room.name == name)
        {
            self.cursors[slot] = index;
        }
    }

    pub fn take_say(&mut self) -> Option<(String, String)> {
        self.pending_say.take()
    }

    pub fn take_join(&mut self) -> Option<String> {
        self.pending_join.take()
    }

    pub fn take_presence(&mut self) -> Option<bool> {
        self.pending_presence.take()
    }

    pub fn ticker_line(&self) -> String {
        let Some(room) = self.rooms.get(self.cursors[View::Chat.index()]) else {
            return "ticker · —".to_owned();
        };
        if let Some(text) = self.tickers.get(&room.name).filter(|text| !text.is_empty()) {
            return format!("ticker · {text}");
        }
        "ticker · —".to_owned()
    }

    pub fn room_members(&self) -> Vec<crate::model::RoomPerson> {
        let Some(room) = self.rooms.get(self.cursors[View::Chat.index()]) else {
            return Vec::new();
        };
        self.members.get(&room.name).cloned().unwrap_or_default()
    }

    pub fn member_cursor(&self) -> usize {
        self.member_cursor
    }

    pub fn person_open(&self) -> bool {
        self.person.is_some()
    }

    pub fn person_name(&self) -> Option<&str> {
        self.person.as_ref().map(|card| card.name.as_str())
    }

    pub fn person_action(&self) -> u8 {
        self.person.as_ref().map(|card| card.action).unwrap_or(0)
    }

    /// Lines above the actions: name, presence, counts, description, likes, and hates.
    pub fn person_summary(&self) -> Vec<String> {
        let Some(name) = self.person_name() else {
            return Vec::new();
        };
        let person = self
            .room_members()
            .into_iter()
            .find(|person| person.name == name);
        let mut lines = vec![name.to_owned()];
        let status = person
            .as_ref()
            .and_then(|person| person.status)
            .map(|status| UserStatus::from_wire(status).label());
        let country = person
            .as_ref()
            .map(|person| person.country.as_str())
            .filter(|country| !country.is_empty());
        match (status, country) {
            (Some(status), Some(country)) => lines.push(format!("{status} · {country}")),
            (Some(status), None) => lines.push(status.to_owned()),
            (None, Some(country)) => lines.push(country.to_owned()),
            (None, None) => {}
        }
        if let Some(person) = person.as_ref()
            && let (Some(files), Some(dirs)) = (person.files, person.dirs)
        {
            lines.push(format!("{files} files · {dirs} dirs"));
        }
        if let Some(info) = self.user_info.get(name)
            && !info.description.is_empty()
        {
            lines.push(info.description.clone());
        }
        if let Some((likes, hates)) = self.interests.get(name) {
            if !likes.is_empty() {
                lines.push(format!("likes  {}", likes.join(", ")));
            }
            if !hates.is_empty() {
                lines.push(format!("hates  {}", hates.join(", ")));
            }
        }
        lines
    }

    pub fn person_actions(&self) -> Vec<String> {
        let friend = self.person_name().is_some_and(|name| self.is_friend(name));
        vec![
            "browse files".to_owned(),
            if friend {
                "already a friend".to_owned()
            } else {
                "add friend".to_owned()
            },
            "ignore".to_owned(),
            "ban".to_owned(),
            "close".to_owned(),
        ]
    }

    pub fn take_inspect(&mut self) -> Option<String> {
        self.pending_inspect.take()
    }

    pub fn chat_for_selected_room(&self) -> Vec<&ChatLine> {
        let Some(room) = self.rooms.get(self.cursors[View::Chat.index()]) else {
            return Vec::new();
        };
        self.chat
            .iter()
            .filter(|line| line.room == room.name)
            .collect()
    }

    pub fn users(&self) -> &[ListedUser] {
        &self.users
    }

    pub fn shares(&self) -> [ShareGroup; 3] {
        self.shares
    }

    pub fn share_paths(&self) -> [&[String]; 3] {
        [
            &self.share_paths[0],
            &self.share_paths[1],
            &self.share_paths[2],
        ]
    }

    pub fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    pub fn bind_picker_open(&self) -> bool {
        self.bind_picker.is_some()
    }

    pub fn bind_rows(&self) -> Vec<String> {
        self.bind_picker
            .as_ref()
            .map(|picker| picker.rows.iter().map(|row| row.label.clone()).collect())
            .unwrap_or_default()
    }

    pub fn bind_cursor(&self) -> usize {
        self.bind_picker
            .as_ref()
            .map(|picker| picker.cursor)
            .unwrap_or(0)
    }

    pub fn folder_field_open(&self) -> bool {
        self.folder_field.is_some()
    }

    pub fn picker_path(&self) -> String {
        self.picker
            .as_ref()
            .map(|picker| picker.cwd.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn picker_cursor(&self) -> usize {
        self.picker
            .as_ref()
            .map(|picker| picker.cursor)
            .unwrap_or(0)
    }

    pub fn picker_rows(&self) -> Vec<PickerLine> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        picker
            .rows
            .iter()
            .map(|row| match row {
                PickerRow::Share => PickerLine {
                    label: "share this folder".to_owned(),
                    kind: PickerKind::Share,
                },
                PickerRow::Up => PickerLine {
                    label: "..".to_owned(),
                    kind: PickerKind::Up,
                },
                PickerRow::Dir(name) => PickerLine {
                    label: format!("{name}/"),
                    kind: PickerKind::Dir,
                },
                PickerRow::File(name) => PickerLine {
                    label: name.clone(),
                    kind: PickerKind::File,
                },
            })
            .collect()
    }

    /// Opens the folder browser at `start`. A missing directory leaves the browser closed.
    pub fn open_folder_picker(&mut self, start: PathBuf) {
        if !start.is_dir() {
            self.notice = "folder is missing".to_owned();
            return;
        }
        match read_folder(&start) {
            Ok(rows) => {
                self.set_cursor_origin(ScrollId::Picker, 0);
                self.scroll_hold.picker = None;
                self.picker = Some(FolderPicker {
                    cwd: start,
                    rows,
                    cursor: 0,
                });
            }
            Err(message) => self.notice = message,
        }
    }

    pub fn path_cursor(&self) -> usize {
        self.path_cursor
    }

    /// Paths shown on the settings shares section, plus where to edit them.
    pub fn share_settings(&self) -> Vec<(String, String)> {
        vec![
            (
                "public folders".to_owned(),
                join_paths(&self.share_paths[0]),
            ),
            ("buddy folders".to_owned(), join_paths(&self.share_paths[1])),
            (
                "trusted folders".to_owned(),
                join_paths(&self.share_paths[2]),
            ),
            (
                "change folders".to_owned(),
                "shares view, enter picks a folder".to_owned(),
            ),
        ]
    }

    pub fn take_rescan(&mut self) -> Option<crate::config::Shares> {
        self.pending_rescan.take()
    }

    pub fn take_forget(&mut self) -> Vec<std::path::PathBuf> {
        std::mem::take(&mut self.pending_forget)
    }

    /// Files a library scan moved into an album-artist folder.
    pub fn take_moves(&mut self) -> Vec<crate::library::Relocation> {
        std::mem::take(&mut self.pending_moves)
    }

    /// The delete confirmation, as the question and the line under it.
    pub fn delete_prompt(&self) -> Option<(&str, &str)> {
        self.confirm
            .as_ref()
            .map(|prompt| (prompt.heading.as_str(), prompt.detail.as_str()))
    }

    pub fn settings(&self) -> &'static [SettingGroup] {
        setting_groups()
    }

    pub fn setting_section(&self) -> &'static str {
        crate::settings::SECTIONS[self.cursor().min(crate::settings::SECTIONS.len() - 1)]
    }

    pub fn setting_lines(&self) -> Vec<(String, String, bool)> {
        let config = self
            .stored
            .as_ref()
            .map(|(_, config)| config.clone())
            .unwrap_or_default();
        let rows = crate::settings::rows(self.cursor().min(crate::settings::SECTIONS.len() - 1));
        rows.iter()
            .enumerate()
            .map(|(index, spec)| {
                let selected = index == self.setting_row.min(rows.len().saturating_sub(1));
                let value = if selected && self.setting_draft.is_some() {
                    let draft = self.setting_draft.as_deref().unwrap_or("");
                    if spec.kind == crate::settings::Kind::Secret {
                        format!("{}█", "*".repeat(draft.chars().count()))
                    } else {
                        format!("{draft}█")
                    }
                } else {
                    crate::settings::display(spec.field, &config)
                };
                (spec.label.to_owned(), value, selected)
            })
            .collect()
    }

    pub fn take_prefs(&mut self) -> Option<crate::settings::LivePrefs> {
        self.pending_prefs.take()
    }

    pub fn signing_in(&self) -> bool {
        self.signing_in && self.banner.is_none()
    }

    pub fn sign_username(&self) -> &str {
        &self.sign_user
    }

    pub fn sign_password_len(&self) -> usize {
        self.sign_password.chars().count()
    }

    pub fn sign_field(&self) -> u8 {
        self.sign_field
    }

    pub fn server_label(&self) -> &str {
        &self.server_label
    }

    /// Config saved by the sign-in form, ready for [`crate::session::Session::spawn`].
    pub fn take_sign_in(&mut self) -> Option<Config> {
        if !self.pending_sign_in {
            return None;
        }
        self.pending_sign_in = false;
        self.stored.as_ref().map(|(_, config)| config.clone())
    }

    pub fn hints(&self) -> &'static str {
        if !self.notice.is_empty() {
            return "";
        }
        if self.confirm.is_some() {
            return "enter removes it   esc cancels";
        }
        if self.person.is_some() {
            return "j/k choose   enter runs it   esc closes";
        }
        if self.signing_in() {
            return "tab switches field  enter signs in  a new name creates the account  esc quits";
        }
        if self.filter_field.is_some() {
            return "enter leaves the filter  esc leaves the filter";
        }
        if self.picker.is_some() {
            return if self.folder_field.is_some() {
                "enter opens   s uses this folder   esc closes"
            } else {
                "enter opens   s shares this folder   esc closes"
            };
        }
        if self.bind_picker.is_some() {
            return "enter selects   esc closes";
        }
        if self.setting_draft.is_some() {
            return "enter saves   esc cancels";
        }
        if self.editing {
            return if self.say_edit {
                "enter sends the line  esc leaves the field"
            } else if self.logged_in() {
                "enter sends the search  esc leaves the field"
            } else {
                "enter keeps the query  esc leaves the field  not logged in"
            };
        }
        match self.view {
            View::Search => {
                "click a filter   ] quality   enter opens an album   d downloads   drag an edge"
            }
            View::Shares => "enter picks a folder   drag an edge   [ ] mark one   x removes it",
            View::Settings => "j/k section   [ ] field   enter edits   x clears   drag an edge",
            View::Chat => "type a message   enter sends it   arrows move   ] people   [ rooms",
            View::Dashboard => {
                "drag an edge   x removes the row   X clears finished   click a row   q quit"
            }
            View::Downloads | View::Uploads => {
                "x removes the row   X clears finished   click a row   wheel scrolls   q quit"
            }
            View::Library => "enter plays   space pauses   s stops   x deletes",
            View::Quality => "[ ] column   j/k row   r checks again",
            View::Users => "drag an edge   click a row   wheel scrolls   click tabs   q quit",
            _ => "click a row   wheel scrolls   click tabs   click ?   q quit",
        }
    }

    fn edit_filter(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.filter_field = None,
            KeyCode::Backspace => {
                if let Some(field) = self.filter_text() {
                    field.pop();
                }
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                if let Some(field) = self.filter_text()
                    && field.chars().count() < QUERY_LIMIT
                {
                    field.push(ch);
                }
            }
            _ => {}
        }
    }

    fn open_share_picker(&mut self) {
        self.open_folder_picker(home_dir());
    }

    fn edit_picker(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.picker = None,
            KeyCode::Char('j') | KeyCode::Down => self.nudge_picker(true),
            KeyCode::Char('k') | KeyCode::Up => self.nudge_picker(false),
            KeyCode::Char('h') | KeyCode::Left => self.picker_up(),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.activate_picker(),
            KeyCode::Char('s') => self.share_picker_folder(),
            _ => {}
        }
    }

    fn nudge_picker(&mut self, down: bool) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let len = picker.rows.len();
        if len == 0 {
            return;
        }
        picker.cursor = if down {
            picker.cursor.saturating_add(1) % len
        } else if picker.cursor == 0 {
            len - 1
        } else {
            picker.cursor - 1
        };
    }

    fn open_bind_picker(&mut self) {
        let rows = crate::settings::bind_choices();
        let current = self
            .stored
            .as_ref()
            .map(|(_, config)| config.bind_address.clone())
            .unwrap_or_default();
        let cursor = rows
            .iter()
            .position(|row| row.address == current)
            .unwrap_or(0);
        self.set_cursor_origin(ScrollId::Bind, 0);
        self.scroll_hold.bind = None;
        self.bind_picker = Some(BindPicker { rows, cursor });
        self.notice.clear();
    }

    fn edit_bind_picker(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.bind_picker = None,
            KeyCode::Char('j') | KeyCode::Down => self.nudge_bind(true),
            KeyCode::Char('k') | KeyCode::Up => self.nudge_bind(false),
            KeyCode::Enter => {
                let index = self
                    .bind_picker
                    .as_ref()
                    .map(|picker| picker.cursor)
                    .unwrap_or(0);
                self.choose_bind(index);
            }
            _ => {}
        }
    }

    fn nudge_bind(&mut self, down: bool) {
        let Some(picker) = &mut self.bind_picker else {
            return;
        };
        let len = picker.rows.len();
        if len == 0 {
            return;
        }
        picker.cursor = if down {
            picker.cursor.saturating_add(1) % len
        } else if picker.cursor == 0 {
            len - 1
        } else {
            picker.cursor - 1
        };
    }

    fn choose_bind(&mut self, index: usize) {
        let Some(address) = self
            .bind_picker
            .as_ref()
            .and_then(|picker| picker.rows.get(index).map(|row| row.address.clone()))
        else {
            return;
        };
        let saved = self.commit_setting(
            |config| {
                config.bind_address = address;
                Ok(())
            },
            true,
        );
        if saved {
            self.bind_picker = None;
        }
    }

    fn activate_picker(&mut self) {
        let index = self
            .picker
            .as_ref()
            .map(|picker| picker.cursor)
            .unwrap_or(0);
        self.activate_picker_row(index);
    }

    fn activate_picker_row(&mut self, index: usize) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if index >= picker.rows.len() {
            return;
        }
        picker.cursor = index;
        let cwd = picker.cwd.clone();
        let row = picker.rows[index].clone();
        match row {
            PickerRow::Share => self.share_picker_folder(),
            PickerRow::Up => self.picker_up(),
            PickerRow::Dir(name) => self.open_folder_picker(cwd.join(name)),
            PickerRow::File(_) => {}
        }
    }

    fn picker_up(&mut self) {
        let Some(parent) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.cwd.parent().map(PathBuf::from))
        else {
            return;
        };
        self.open_folder_picker(parent);
    }

    fn share_picker_folder(&mut self) {
        let Some(path) = self
            .picker
            .as_ref()
            .map(|picker| picker.cwd.to_string_lossy().into_owned())
        else {
            return;
        };
        self.picker = None;
        if let Some(field) = self.folder_field.take() {
            self.commit_setting(
                |config| {
                    crate::settings::set_folder(field, config, path);
                    Ok(())
                },
                false,
            );
            return;
        }
        self.add_share_folder(path);
    }

    fn add_share_folder(&mut self, path: String) {
        if self.stored.is_none() {
            self.notice = "no config file".to_owned();
            return;
        }
        if path.is_empty() || !std::path::Path::new(&path).is_dir() {
            self.notice = "folder is missing".to_owned();
            return;
        }
        let group = self.cursor().min(2);
        if self.share_paths[group].iter().any(|item| item == &path) {
            self.notice = "folder is already shared".to_owned();
            return;
        }
        self.share_paths[group].push(path);
        self.path_cursor = self.share_paths[group].len() - 1;
        self.save_shares("folder saved");
    }

    fn remove_share_folder(&mut self) {
        if self.stored.is_none() {
            self.notice = "no config file".to_owned();
            return;
        }
        let group = self.cursor().min(2);
        if self.share_paths[group].is_empty() {
            self.notice = "no folder to remove".to_owned();
            return;
        }
        let index = self.path_cursor.min(self.share_paths[group].len() - 1);
        self.share_paths[group].remove(index);
        self.path_cursor = self
            .path_cursor
            .min(self.share_paths[group].len().saturating_sub(1));
        self.save_shares("folder removed");
    }

    fn nudge_share_path(&mut self, down: bool) {
        let group = self.cursor().min(2);
        let len = self.share_paths[group].len();
        if len == 0 {
            return;
        }
        self.path_cursor = if down {
            self.path_cursor.saturating_add(1) % len
        } else if self.path_cursor == 0 {
            len - 1
        } else {
            self.path_cursor - 1
        };
    }

    fn remember_public_share(&mut self, path: &Path) {
        let text = path.to_string_lossy().into_owned();
        if self
            .share_paths
            .iter()
            .any(|group| group.iter().any(|item| item == &text))
        {
            return;
        }
        if self.stored.is_none() {
            return;
        }
        self.share_paths[0].push(text);
        self.save_shares("sharing the download folder");
    }

    fn save_shares(&mut self, notice: &str) {
        let Some((path, config)) = &mut self.stored else {
            self.notice = "no config file".to_owned();
            return;
        };
        config.shares.public = self.share_paths[0].iter().map(PathBuf::from).collect();
        config.shares.buddy = self.share_paths[1].iter().map(PathBuf::from).collect();
        config.shares.trusted = self.share_paths[2].iter().map(PathBuf::from).collect();
        if let Err(err) = config.save(path) {
            self.notice = err.to_string();
            return;
        }
        self.pending_rescan = Some(config.shares.clone());
        self.notice = notice.to_owned();
    }

    fn current_spec(&self) -> Option<&'static crate::settings::Spec> {
        let rows = crate::settings::rows(self.cursor().min(crate::settings::SECTIONS.len() - 1));
        rows.get(self.setting_row.min(rows.len().saturating_sub(1)))
    }

    fn nudge_setting(&mut self, down: bool) {
        let len =
            crate::settings::rows(self.cursor().min(crate::settings::SECTIONS.len() - 1)).len();
        if len == 0 {
            return;
        }
        self.setting_row = if down {
            self.setting_row.saturating_add(1) % len
        } else if self.setting_row == 0 {
            len - 1
        } else {
            self.setting_row - 1
        };
    }

    fn activate_setting(&mut self) {
        let Some(spec) = self.current_spec() else {
            return;
        };
        let field = spec.field;
        let restart = spec.restart;
        match spec.kind {
            crate::settings::Kind::Bool => {
                self.commit_setting(
                    |config| {
                        if crate::settings::toggle(field, config) {
                            Ok(())
                        } else {
                            Err("not a switch".to_owned())
                        }
                    },
                    restart,
                );
            }
            crate::settings::Kind::Queue => {
                self.commit_setting(
                    |config| {
                        if crate::settings::cycle(field, config) {
                            Ok(())
                        } else {
                            Err("not a choice".to_owned())
                        }
                    },
                    restart,
                );
            }
            crate::settings::Kind::Folder => {
                self.folder_field = Some(field);
                self.open_share_picker();
            }
            crate::settings::Kind::Interface => self.open_bind_picker(),
            crate::settings::Kind::Jump => self.set_view(View::Shares),
            crate::settings::Kind::Text
            | crate::settings::Kind::Secret
            | crate::settings::Kind::Port
            | crate::settings::Kind::Count
            | crate::settings::Kind::Slots
            | crate::settings::Kind::Minutes
            | crate::settings::Kind::Kib
            | crate::settings::Kind::Names
            | crate::settings::Kind::Pairs => {
                let config = self
                    .stored
                    .as_ref()
                    .map(|(_, config)| config.clone())
                    .unwrap_or_default();
                self.setting_draft = Some(crate::settings::edit_seed(spec.field, &config));
                self.notice.clear();
            }
        }
    }

    fn edit_setting(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.setting_draft = None,
            KeyCode::Enter => {
                let Some(text) = self.setting_draft.take() else {
                    return;
                };
                let Some(spec) = self.current_spec() else {
                    return;
                };
                let restart = spec.restart;
                let field = spec.field;
                self.commit_setting(
                    |config| crate::settings::apply_text(field, config, &text),
                    restart,
                );
            }
            KeyCode::Backspace => {
                if let Some(draft) = &mut self.setting_draft {
                    draft.pop();
                }
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                if let Some(draft) = &mut self.setting_draft
                    && draft.chars().count() < 2000
                {
                    draft.push(ch);
                }
            }
            _ => {}
        }
    }

    fn clear_setting(&mut self) {
        let Some(spec) = self.current_spec() else {
            return;
        };
        let restart = spec.restart;
        let field = spec.field;
        self.commit_setting(
            |config| {
                if crate::settings::clear(field, config) {
                    Ok(())
                } else {
                    Err("type a new value".to_owned())
                }
            },
            restart,
        );
    }

    fn commit_setting(
        &mut self,
        change: impl FnOnce(&mut Config) -> Result<(), String>,
        restart: bool,
    ) -> bool {
        let Some((path, config)) = &mut self.stored else {
            self.notice = "no config file".to_owned();
            return false;
        };
        let mut next = config.clone();
        if let Err(err) = change(&mut next) {
            self.notice = err;
            return false;
        }
        if let Err(err) = next.save(path) {
            self.notice = err.to_string();
            return false;
        }
        *config = next;
        self.auto_away_minutes = config.auto_away_minutes;
        self.pending_prefs = Some(crate::settings::LivePrefs::from_config(config));
        self.pending_lists = Some((
            config.banned.clone(),
            config.ignored.clone(),
            config
                .buddies
                .iter()
                .filter(|buddy| buddy.prioritized)
                .map(|buddy| buddy.name.clone())
                .collect(),
        ));
        self.notice = if restart {
            "saved. restart to apply".to_owned()
        } else {
            "saved".to_owned()
        };
        true
    }

    fn filter_text(&mut self) -> Option<&mut String> {
        match self.filter_field {
            Some(6) => Some(&mut self.country_filter),
            Some(7) => Some(&mut self.include),
            Some(8) => Some(&mut self.exclude),
            _ => None,
        }
    }

    fn begin_say(&mut self, ch: char) {
        self.editing = true;
        self.say_edit = true;
        self.notice.clear();
        if !ch.is_control() && self.say_draft.chars().count() < QUERY_LIMIT {
            self.say_draft.push(ch);
        }
    }

    fn edit(&mut self, key: KeyEvent) {
        if self.say_edit {
            match key.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.say_edit = false;
                }
                KeyCode::Enter => {
                    self.editing = false;
                    self.say_edit = false;
                    self.submit_say();
                }
                KeyCode::Backspace => {
                    let end = self
                        .say_draft
                        .char_indices()
                        .next_back()
                        .map(|(index, _)| index);
                    if let Some(index) = end {
                        self.say_draft.truncate(index);
                    }
                    self.notice.clear();
                }
                KeyCode::Char(ch) if self.say_draft.chars().count() < QUERY_LIMIT => {
                    self.say_draft.push(ch);
                    self.notice.clear();
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc => self.editing = false,
            KeyCode::Enter => {
                self.editing = false;
                self.submit_search();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.notice.clear();
            }
            KeyCode::Char(ch) if self.query.chars().count() < QUERY_LIMIT => {
                self.query.push(ch);
                self.notice.clear();
            }
            _ => {}
        }
    }

    fn open_sign_in(&mut self) {
        self.signing_in = true;
        self.sign_busy = false;
        if self.sign_user.is_empty() {
            self.sign_user = self.account.clone();
        }
    }

    fn enter_live(&mut self) {
        if self.feed == Feed::Live {
            return;
        }
        self.feed = Feed::Live;
        self.listen_ip = None;
        let arrived = std::mem::take(&mut self.live);
        self.transfers = arrived;
        self.hits.clear();
        self.open_folders.clear();
        self.note_hits();
        self.note_open();
        self.asked_folders.clear();
        self.album_downloads.clear();
        self.browse.clear();
        self.browse_base = 0;
        self.rooms.clear();
        self.chat.clear();
        self.users.clear();
        self.members.clear();
        self.member_cursor = 0;
        self.member_focus = false;
        self.person = None;
        self.tickers.clear();
        self.shares = share_groups();
        self.restore_configured_users();
        self.down.clear();
        self.up.clear();
        self.down.push(0);
        self.up.push(0);
        self.down_done = 0;
        self.up_done = 0;
    }

    fn restore_configured_users(&mut self) {
        let Some((_, config)) = &self.stored else {
            return;
        };
        let config = config.clone();
        for buddy in &config.buddies {
            self.push_user(ListedUser {
                list: UserList::Buddy,
                name: buddy.name.clone(),
                status: UserStatus::Offline,
                country: "—".to_owned(),
                note: buddy.note.clone(),
                notify: buddy.notify,
                prioritized: buddy.prioritized,
                trusted: buddy.trusted,
                last_seen: "—".to_owned(),
            });
        }
        for name in &config.ignored {
            self.push_user(listed(UserList::Ignored, name));
        }
        for name in &config.banned {
            self.push_user(listed(UserList::Banned, name));
        }
    }

    fn edit_sign_in(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                self.sign_field = if self.sign_field == 0 { 1 } else { 0 };
            }
            KeyCode::Enter if self.sign_field == 0 => {
                if self.sign_user.trim().is_empty() {
                    self.notice = "enter a username".to_owned();
                } else {
                    self.sign_field = 1;
                    self.notice.clear();
                }
            }
            KeyCode::Enter => self.submit_sign_in(),
            KeyCode::Backspace => {
                if self.sign_field == 0 {
                    self.sign_user.pop();
                } else {
                    self.sign_password.pop();
                }
                self.notice.clear();
            }
            KeyCode::Char(ch) if !ch.is_control() => {
                let field = if self.sign_field == 0 {
                    &mut self.sign_user
                } else {
                    &mut self.sign_password
                };
                if field.chars().count() < QUERY_LIMIT {
                    field.push(ch);
                    self.notice.clear();
                }
            }
            _ => {}
        }
    }

    fn submit_sign_in(&mut self) {
        if self.sign_busy {
            return;
        }
        let username = self.sign_user.trim().to_owned();
        if username.is_empty() {
            self.notice = "enter a username".to_owned();
            self.sign_field = 0;
            return;
        }
        let Some((path, config)) = &mut self.stored else {
            self.notice = "no config file".to_owned();
            return;
        };
        config.username.clone_from(&username);
        config.set_password(self.sign_password.clone());
        if let Err(err) = config.save(path) {
            self.notice = err.to_string();
            return;
        }
        self.account = username;
        self.sign_busy = true;
        self.pending_sign_in = true;
        self.notice = "signing in…".to_owned();
    }

    fn request_join(&mut self) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        let Some(room) = self.rooms.get(self.cursors[View::Chat.index()]) else {
            self.notice = "no room selected".to_owned();
            return;
        };
        self.pending_join = Some(room.name.clone());
        self.notice.clear();
    }

    fn submit_say(&mut self) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        let Some(room) = self.rooms.get(self.cursors[View::Chat.index()]) else {
            self.notice = "no room selected".to_owned();
            return;
        };
        if self.say_draft.trim().is_empty() {
            self.notice = "say is empty".to_owned();
            return;
        }
        self.pending_say = Some((room.name.clone(), self.say_draft.clone()));
        self.say_draft.clear();
        self.notice.clear();
    }

    fn note_activity(&mut self) {
        self.idle = std::time::Duration::ZERO;
        if self.away_sent {
            self.away_sent = false;
            self.pending_presence = Some(false);
        }
    }

    fn consider_away(&mut self) {
        if self.auto_away_minutes == 0 || self.away_sent {
            return;
        }
        let limit = std::time::Duration::from_secs(u64::from(self.auto_away_minutes) * 60);
        if self.idle >= limit {
            self.away_sent = true;
            self.pending_presence = Some(true);
        }
    }

    fn submit_search(&mut self) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        if self.query.trim().is_empty() {
            self.notice = "search query is empty".to_owned();
            return;
        }
        if self.mode == SearchMode::Wishlist {
            self.notice = "wishlist searches wait for the server interval".to_owned();
            return;
        }
        if self.mode == SearchMode::Room && self.room.is_empty() {
            self.notice = "no room selected".to_owned();
            return;
        }
        if self.mode == SearchMode::User && self.user_target.is_empty() {
            self.notice = "no user selected".to_owned();
            return;
        }
        let query = self.query.trim().to_owned();
        self.album_retry = None;
        self.album_retry_queue.clear();
        self.file_retry = None;
        self.file_retry_queue.clear();
        if self.feed == Feed::Live {
            self.hits.clear();
            self.open_folders.clear();
            self.note_hits();
            self.note_open();
            self.asked_folders.clear();
            self.album_downloads.clear();
            self.cursors[View::Search.index()] = 0;
            self.set_list_origin(View::Search.index(), 0);
            self.scroll_hold.list[View::Search.index()] = None;
            self.shown_search = ShownSearch::Waiting;
        }
        self.pending = Some(crate::session::SearchRequest {
            mode: self.mode,
            query: query.clone(),
            room: self.room.clone(),
            user: self.user_target.clone(),
        });
        self.notice = format!("searching for {query}");
    }

    fn queue_download(&mut self) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        if self.view == View::Search {
            let files = self.selected_files();
            if files.is_empty() {
                self.notice = "no file selected".to_owned();
                return;
            }
            if files[0].0.starts_with("preview-") {
                self.notice = "preview row".to_owned();
                return;
            }
            let count = files.len();
            let album = if self.selected_is_folder() {
                self.search_lines()
                    .get(self.cursor())
                    .map(|line| (line.user.clone(), line.path.clone()))
            } else {
                None
            };
            if let Some((user, path)) = &album {
                let key = folder_key(user, path);
                self.album_downloads.insert(key);
                self.ask_folder(user, path);
            }
            let folder = album.map(|(_, path)| path);
            for (user, path, size) in files {
                self.pending_downloads
                    .push(crate::session::DownloadRequest {
                        user,
                        path,
                        size,
                        folder: folder.clone(),
                        stage: false,
                    });
            }
            self.notice = if count == 1 {
                "fetching 1 file".to_owned()
            } else {
                format!("fetching {count} files")
            };
            return;
        }
        let (user, path, size) = {
            let Some(row) = self.browse.get(self.cursor()) else {
                self.notice = "no file selected".to_owned();
                return;
            };
            let Some(size) = row.size else {
                self.notice = "no file selected".to_owned();
                return;
            };
            if self.user_target.is_empty() {
                self.notice = "no user selected".to_owned();
                return;
            }
            (self.user_target.clone(), row.name.clone(), size)
        };
        self.pending_downloads
            .push(crate::session::DownloadRequest {
                user,
                path,
                size,
                folder: None,
                stage: false,
            });
        self.notice.clear();
    }

    fn selected_is_folder(&self) -> bool {
        self.search_lines()
            .get(self.cursor())
            .is_some_and(|line| line.folder)
    }

    fn selected_files(&self) -> Vec<(String, String, u64)> {
        let lines = self.search_lines();
        let Some(line) = lines.get(self.cursor()) else {
            return Vec::new();
        };
        if !line.folder {
            return vec![(line.user.clone(), line.path.clone(), line.size)];
        }
        self.visible_hits()
            .into_iter()
            .filter(|hit| hit.user == line.user && parent_dir(&hit.path) == line.path)
            .map(|hit| (hit.user.clone(), hit.path.clone(), hit.size))
            .collect()
    }

    fn toggle_folder(&mut self, index: usize) {
        let (user, path) = {
            let lines = self.search_lines();
            let Some(line) = lines.get(index) else {
                return;
            };
            if !line.folder {
                return;
            }
            (line.user.clone(), line.path.clone())
        };
        let key = folder_key(&user, &path);
        if self.open_folders.contains(&key) {
            self.open_folders.remove(&key);
        } else {
            self.open_folders.insert(key);
            self.ask_folder(&user, &path);
        }
        self.note_open();
        let len = self.list_len();
        let cursor = &mut self.cursors[self.view.index()];
        if len == 0 {
            *cursor = 0;
        } else if *cursor >= len {
            *cursor = len - 1;
        }
    }

    fn begin_album_retry(&mut self, album: String, exclude: String) {
        if album.is_empty() {
            return;
        }
        if self.album_retry.is_some() || self.file_retry.is_some() {
            let queued = self.album_retry_queue.iter().any(|(name, user)| {
                name.eq_ignore_ascii_case(&album) && user.eq_ignore_ascii_case(&exclude)
            });
            if !queued {
                self.album_retry_queue.push((album, exclude));
            }
            return;
        }
        if !self.logged_in() {
            self.notice = format!("{album} failed");
            return;
        }
        self.album_retry = Some(AlbumRetry {
            album: album.clone(),
            exclude,
            armed: true,
            token: None,
            hits: Vec::new(),
            wait: 0,
        });
        self.mode = SearchMode::Global;
        self.query.clone_from(&album);
        self.pending = Some(crate::session::SearchRequest {
            mode: SearchMode::Global,
            query: album.clone(),
            room: String::new(),
            user: String::new(),
        });
        self.notice = format!("failed. searching for {album}");
    }

    fn note_album_hit(&mut self, token: u32, hit: &SearchHit) {
        if self.is_blocked(&hit.user) {
            return;
        }
        let Some(retry) = &mut self.album_retry else {
            return;
        };
        if retry.token != Some(token) {
            return;
        }
        if hit.user.eq_ignore_ascii_case(&retry.exclude) {
            return;
        }
        let folder = parent_dir(&hit.path);
        if !folder_album(folder).eq_ignore_ascii_case(&retry.album) {
            return;
        }
        retry.hits.push(hit.clone());
    }

    fn advance_album_retry(&mut self) {
        let ready = match &mut self.album_retry {
            Some(retry) if retry.token.is_some() => {
                if retry.wait > 0 {
                    retry.wait -= 1;
                }
                retry.wait == 0
            }
            _ => false,
        };
        if ready {
            self.finish_album_retry();
        }
    }

    fn finish_album_retry(&mut self) {
        let Some(retry) = self.album_retry.take() else {
            return;
        };
        match choose_album_source(&retry.album, &retry.exclude, &retry.hits) {
            Some((user, folder, files)) => {
                self.album_downloads.insert(folder_key(&user, &folder));
                self.ask_folder(&user, &folder);
                let count = files.len();
                for file in files {
                    self.pending_downloads
                        .push(crate::session::DownloadRequest {
                            user: file.user,
                            path: file.path,
                            size: file.size,
                            folder: Some(folder.clone()),
                            stage: false,
                        });
                }
                self.notice = format!("fetching {count} files of {} from {user}", retry.album);
            }
            None => {
                self.mark_album_unavailable(&retry.exclude, &retry.album);
            }
        }
        self.start_next_album_retry();
        self.start_next_file_retry();
    }

    fn mark_album_unavailable(&mut self, user: &str, album: &str) {
        for rows in [&mut self.transfers, &mut self.live] {
            for transfer in rows.iter_mut() {
                if transfer.direction != Direction::Download
                    || transfer.state != TransferState::Failed
                    || !transfer.user.eq_ignore_ascii_case(user)
                {
                    continue;
                }
                if !folder_album(parent_dir(&transfer.path)).eq_ignore_ascii_case(album) {
                    continue;
                }
                transfer.detail = "Unavailable".to_owned();
            }
        }
        self.pending_unavailable
            .push((user.to_owned(), album.to_owned()));
        self.notice = "Failed - Unavailable".to_owned();
    }

    fn start_next_album_retry(&mut self) {
        if self.album_retry.is_some() || self.file_retry.is_some() {
            return;
        }
        if self.album_retry_queue.is_empty() {
            return;
        }
        let (album, exclude) = self.album_retry_queue.remove(0);
        self.begin_album_retry(album, exclude);
    }

    fn begin_file_retry(
        &mut self,
        file: String,
        exclude: String,
        path: String,
        folder: Option<String>,
    ) {
        if file.is_empty() {
            return;
        }
        let same =
            |job: &FileRetryJob| job.path == path && job.exclude.eq_ignore_ascii_case(&exclude);
        if self
            .file_retry
            .as_ref()
            .is_some_and(|retry| retry.path == path && retry.exclude.eq_ignore_ascii_case(&exclude))
        {
            return;
        }
        if self.album_retry.is_some() || self.file_retry.is_some() {
            if !self.file_retry_queue.iter().any(same) {
                self.file_retry_queue.push(FileRetryJob {
                    file,
                    exclude,
                    path,
                    folder,
                });
            }
            return;
        }
        if !self.logged_in() {
            self.notice = format!("{file} failed");
            return;
        }
        self.file_retry = Some(FileRetry {
            file: file.clone(),
            exclude,
            path,
            folder,
            armed: true,
            token: None,
            hits: Vec::new(),
            wait: 0,
        });
        self.mode = SearchMode::Global;
        self.query.clone_from(&file);
        self.pending = Some(crate::session::SearchRequest {
            mode: SearchMode::Global,
            query: file.clone(),
            room: String::new(),
            user: String::new(),
        });
        self.notice = format!("failed. searching for {file}");
    }

    fn note_file_hit(&mut self, token: u32, hit: &SearchHit) {
        if self.is_blocked(&hit.user) {
            return;
        }
        let Some(retry) = &mut self.file_retry else {
            return;
        };
        if retry.token != Some(token) {
            return;
        }
        if hit.user.eq_ignore_ascii_case(&retry.exclude) {
            return;
        }
        if !path_file(&hit.path).eq_ignore_ascii_case(&retry.file) {
            return;
        }
        retry.hits.push(hit.clone());
    }

    fn advance_file_retry(&mut self) {
        let ready = match &mut self.file_retry {
            Some(retry) if retry.token.is_some() => {
                if retry.wait > 0 {
                    retry.wait -= 1;
                }
                retry.wait == 0
            }
            _ => false,
        };
        if ready {
            self.finish_file_retry();
        }
    }

    fn finish_file_retry(&mut self) {
        let Some(retry) = self.file_retry.take() else {
            return;
        };
        match choose_file_source(&retry.file, &retry.exclude, &retry.hits) {
            Some(hit) => {
                let user = hit.user.clone();
                let path = hit.path.clone();
                let size = hit.size;
                self.drop_unshared(&retry.exclude, &retry.path);
                self.pending_downloads
                    .push(crate::session::DownloadRequest {
                        user: user.clone(),
                        path,
                        size,
                        folder: retry.folder.clone(),
                        stage: false,
                    });
                self.notice = format!("fetching {} from {user}", retry.file);
            }
            None => self.mark_file_unavailable(&retry.exclude, &retry.path),
        }
        self.start_next_file_retry();
        self.start_next_album_retry();
    }

    fn drop_unshared(&mut self, user: &str, path: &str) {
        let drop_row = |row: &Transfer| {
            row.direction == Direction::Download && row.user == user && row.path == path
        };
        self.transfers.retain(|row| !drop_row(row));
        self.live.retain(|row| !drop_row(row));
        self.pending_cancel
            .push((Direction::Download, user.to_owned(), path.to_owned()));
    }

    fn mark_file_unavailable(&mut self, user: &str, path: &str) {
        for rows in [&mut self.transfers, &mut self.live] {
            for transfer in rows.iter_mut() {
                if transfer.direction != Direction::Download
                    || !matches!(
                        transfer.state,
                        TransferState::Failed | TransferState::LastTryFailed
                    )
                    || transfer.user != user
                    || transfer.path != path
                {
                    continue;
                }
                transfer.state = TransferState::Failed;
                transfer.detail = "Unavailable".to_owned();
            }
        }
        self.pending_file_unavailable
            .push((user.to_owned(), path.to_owned()));
        self.notice = "Failed - Unavailable".to_owned();
    }

    fn start_next_file_retry(&mut self) {
        if self.album_retry.is_some() || self.file_retry.is_some() {
            return;
        }
        if self.file_retry_queue.is_empty() {
            return;
        }
        let job = self.file_retry_queue.remove(0);
        self.begin_file_retry(job.file, job.exclude, job.path, job.folder);
    }

    fn ask_folder(&mut self, user: &str, directory: &str) {
        if user.starts_with("preview-") || !self.logged_in() {
            return;
        }
        if !self.asked_folders.insert(folder_key(user, directory)) {
            return;
        }
        self.pending_folders
            .push((user.to_owned(), directory.to_owned()));
        self.notice = format!("fetching {directory}");
    }

    fn request_browse(&mut self) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        let user = if self.view == View::Users {
            let Some(user) = self.users.get(self.cursor()) else {
                self.notice = "no user selected".to_owned();
                return;
            };
            user.name.clone()
        } else {
            self.user_target.clone()
        };
        if user.is_empty() {
            self.notice = "no user selected".to_owned();
            return;
        }
        if user.starts_with("preview-") {
            self.notice = "preview row".to_owned();
            return;
        }
        self.user_target = user.clone();
        self.pending_browse = Some(user);
        self.notice.clear();
    }

    fn toggle_flag(&mut self, flag: Flag) {
        let Some(name) = self.users.get(self.cursor()).map(|user| user.name.clone()) else {
            self.notice = "no user selected".to_owned();
            return;
        };
        if name.starts_with("preview-") {
            self.notice = "preview row".to_owned();
            return;
        }
        let updated = {
            let Some((path, config)) = &mut self.stored else {
                self.notice = "no config file".to_owned();
                return;
            };
            let Some(buddy) = config.buddies.iter_mut().find(|buddy| buddy.name == name) else {
                self.notice = "not a buddy".to_owned();
                return;
            };
            match flag {
                Flag::Notify => buddy.notify = !buddy.notify,
                Flag::Priority => buddy.prioritized = !buddy.prioritized,
                Flag::Trusted => buddy.trusted = !buddy.trusted,
            }
            let notify = buddy.notify;
            let prioritized = buddy.prioritized;
            let trusted = buddy.trusted;
            let saved = config.save(path).err().map(|err| err.to_string());
            let lists = (
                config.banned.clone(),
                config.ignored.clone(),
                config
                    .buddies
                    .iter()
                    .filter(|buddy| buddy.prioritized)
                    .map(|buddy| buddy.name.clone())
                    .collect::<Vec<_>>(),
            );
            (saved, lists, notify, prioritized, trusted)
        };
        let (saved, lists, notify, prioritized, trusted) = updated;
        if let Some(err) = saved {
            self.notice = err;
            return;
        }
        self.pending_lists = Some(lists);
        if let Some(user) = self.users.iter_mut().find(|user| user.name == name) {
            user.notify = notify;
            user.prioritized = prioritized;
            user.trusted = trusted;
        }
        self.notice.clear();
    }

    fn note_person(
        &mut self,
        name: &str,
        status: Option<u32>,
        country: Option<&str>,
        files: Option<u32>,
        dirs: Option<u32>,
    ) {
        for people in self.members.values_mut() {
            for person in people.iter_mut() {
                if person.name != name {
                    continue;
                }
                if let Some(status) = status {
                    person.status = Some(status);
                }
                if let Some(country) = country.filter(|country| !country.is_empty()) {
                    person.country = country.to_owned();
                }
                if let Some(files) = files {
                    person.files = Some(files);
                }
                if let Some(dirs) = dirs {
                    person.dirs = Some(dirs);
                }
            }
        }
    }

    fn open_person(&mut self) {
        self.open_person_at(self.member_cursor);
    }

    fn open_person_at(&mut self, index: usize) {
        let Some(person) = self.room_members().get(index).cloned() else {
            return;
        };
        self.member_cursor = index;
        self.member_focus = true;
        let name = person.name.clone();
        self.person = Some(PersonCard {
            name: name.clone(),
            action: 0,
        });
        if self.logged_in() && !name.starts_with("preview-") {
            self.pending_inspect = Some(name);
        }
    }

    pub fn highlight_member(&mut self, index: usize) {
        let len = self.room_members().len();
        if len == 0 {
            return;
        }
        self.member_cursor = index.min(len - 1);
        self.member_focus = true;
    }

    pub fn nudge_member(&mut self, down: bool) {
        let len = self.room_members().len();
        if len == 0 {
            self.member_cursor = 0;
            return;
        }
        if self.member_cursor >= len {
            self.member_cursor = len - 1;
        }
        if down {
            if self.member_cursor + 1 < len {
                self.member_cursor += 1;
            }
        } else {
            self.member_cursor = self.member_cursor.saturating_sub(1);
        }
        self.member_focus = true;
    }

    fn nudge_person(&mut self, down: bool) {
        let Some(card) = &mut self.person else {
            return;
        };
        if down {
            card.action = card.action.saturating_add(1).min(4);
        } else {
            card.action = card.action.saturating_sub(1);
        }
    }

    fn run_person_action(&mut self) {
        let Some(name) = self.person.as_ref().map(|card| card.name.clone()) else {
            return;
        };
        let action = self.person.as_ref().map(|card| card.action).unwrap_or(4);
        match action {
            0 => self.browse_person(&name),
            1 => self.add_friend(&name),
            2 => {
                self.apply_list(&name, UserList::Ignored);
                self.person = None;
            }
            3 => {
                self.apply_list(&name, UserList::Banned);
                self.person = None;
            }
            _ => self.person = None,
        }
    }

    fn browse_person(&mut self, name: &str) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        if name.starts_with("preview-") {
            self.notice = "preview row".to_owned();
            return;
        }
        self.user_target = name.to_owned();
        self.pending_browse = Some(name.to_owned());
        self.person = None;
        self.set_view(View::Browse);
        self.notice.clear();
    }

    fn is_friend(&self, name: &str) -> bool {
        self.stored
            .as_ref()
            .is_some_and(|(_, config)| config.buddies.iter().any(|buddy| buddy.name == name))
    }

    fn add_friend(&mut self, name: &str) {
        if !self.logged_in() {
            self.notice = "not logged in".to_owned();
            return;
        }
        if name.starts_with("preview-") {
            self.notice = "preview row".to_owned();
            return;
        }
        if self.is_friend(name) {
            self.notice = "already a friend".to_owned();
            return;
        }
        let updated = {
            let Some((path, config)) = &mut self.stored else {
                self.notice = "no config file".to_owned();
                return;
            };
            config.ignored.retain(|item| item != name);
            config.banned.retain(|item| item != name);
            config.buddies.push(crate::config::Buddy {
                name: name.to_owned(),
                note: String::new(),
                notify: false,
                prioritized: false,
                trusted: false,
            });
            let saved = config.save(path).err().map(|err| err.to_string());
            let lists = (
                config.banned.clone(),
                config.ignored.clone(),
                config
                    .buddies
                    .iter()
                    .filter(|buddy| buddy.prioritized)
                    .map(|buddy| buddy.name.clone())
                    .collect::<Vec<_>>(),
            );
            (saved, lists)
        };
        let (saved, lists) = updated;
        if let Some(err) = saved {
            self.notice = err;
            return;
        }
        self.pending_lists = Some(lists);
        self.pending_inspect = Some(name.to_owned());
        self.users.retain(|user| user.name != name);
        self.push_user(listed(UserList::Buddy, name));
        self.notice = format!("added {name}");
    }

    fn block_selected(&mut self, list: UserList) {
        let name = if self.view == View::Search {
            let found = {
                let lines = self.search_lines();
                lines.get(self.cursor()).map(|line| line.user.clone())
            };
            let Some(name) = found else {
                self.notice = "no file selected".to_owned();
                return;
            };
            name
        } else {
            let Some(user) = self.users.get(self.cursor()) else {
                self.notice = "no user selected".to_owned();
                return;
            };
            user.name.clone()
        };
        self.apply_list(&name, list);
    }

    fn apply_list(&mut self, name: &str, list: UserList) {
        if name.starts_with("preview-") {
            self.notice = "preview row".to_owned();
            return;
        }
        let updated = {
            let Some((path, config)) = &mut self.stored else {
                self.notice = "no config file".to_owned();
                return;
            };
            let already = match list {
                UserList::Banned => config.banned.iter().any(|item| item == name),
                UserList::Ignored => config.ignored.iter().any(|item| item == name),
                UserList::Buddy => false,
            };
            config.buddies.retain(|buddy| buddy.name != name);
            config.ignored.retain(|item| item != name);
            config.banned.retain(|item| item != name);
            if !already {
                match list {
                    UserList::Banned => config.banned.push(name.to_owned()),
                    UserList::Ignored => config.ignored.push(name.to_owned()),
                    UserList::Buddy => {}
                }
            }
            let saved = config.save(path).err().map(|err| err.to_string());
            let lists = (
                config.banned.clone(),
                config.ignored.clone(),
                config
                    .buddies
                    .iter()
                    .filter(|buddy| buddy.prioritized)
                    .map(|buddy| buddy.name.clone())
                    .collect::<Vec<_>>(),
            );
            (saved, lists, already)
        };
        let (saved, lists, already) = updated;
        if let Some(err) = saved {
            self.notice = err;
            return;
        }
        self.pending_lists = Some(lists);
        self.users.retain(|user| user.name != name);
        if !already {
            self.users.push(listed(list, name));
        }
        if !already && matches!(list, UserList::Banned | UserList::Ignored) {
            self.hits.retain(|hit| hit.user != name);
            self.note_hits();
        }
        self.notice.clear();
    }

    fn note_status(&mut self, name: &str, status: u32, country: Option<&str>) {
        let status = UserStatus::from_wire(status);
        if let Some(user) = self.users.iter_mut().find(|user| user.name == name) {
            user.status = status;
            if let Some(country) = country.filter(|country| !country.is_empty()) {
                user.country = country.to_owned();
            }
        }
    }

    fn push_user(&mut self, user: ListedUser) {
        if self.users.iter().any(|found| found.name == user.name) {
            return;
        }
        self.users.push(user);
    }

    fn is_blocked(&self, name: &str) -> bool {
        self.stored.as_ref().is_some_and(|(_, config)| {
            config.banned.iter().any(|item| item == name)
                || config.ignored.iter().any(|item| item == name)
        })
    }

    fn select(&mut self, index: usize) {
        self.editing = false;
        let len = self.list_len();
        if len == 0 {
            return;
        }
        let slot = self.view.index();
        let next = index.min(len - 1);
        let before = self.cursors[slot];
        self.cursors[slot] = next;
        if self.view == View::Chat && before != next {
            self.reset_room_scroll();
        }
    }

    fn nudge(&mut self, down: bool) {
        let slot = self.view.index();
        let before = self.cursors[slot];
        let len = self.list_len();
        if len == 0 {
            self.cursors[slot] = 0;
            return;
        }
        if self.cursors[slot] >= len {
            self.cursors[slot] = len - 1;
        }
        if down {
            if self.cursors[slot] + 1 < len {
                self.cursors[slot] += 1;
            }
        } else {
            self.cursors[slot] = self.cursors[slot].saturating_sub(1);
        }
        if self.view == View::Chat && self.cursors[slot] != before {
            self.reset_room_scroll();
        }
    }

    fn remove_transfer(&mut self) {
        let Some((direction, user, path)) = self.highlighted_transfer() else {
            return;
        };
        if direction == Direction::Download {
            self.pending_downloads
                .retain(|item| item.user != user || item.path != path);
        }
        let preview = user.starts_with("preview-") || !self.logged_in();
        self.transfers
            .retain(|row| !(row.direction == direction && row.user == user && row.path == path));
        self.live
            .retain(|row| !(row.direction == direction && row.user == user && row.path == path));
        if !preview {
            self.pending_cancel.push((direction, user, path));
        }
        self.notice = "removed".to_owned();
        self.clamp_cursor();
    }

    fn clear_finished(&mut self) {
        let (downloads, uploads) = match self.view {
            View::Downloads => (true, false),
            View::Uploads => (false, true),
            View::Dashboard => (true, true),
            _ => return,
        };
        let drop_row = |row: &Transfer| {
            let chosen = match row.direction {
                Direction::Download => downloads,
                Direction::Upload => uploads,
            };
            chosen && row.state == TransferState::Finished
        };
        self.transfers.retain(|row| !drop_row(row));
        self.live.retain(|row| !drop_row(row));
        if self.logged_in() {
            self.pending_clear_finished = Some((downloads, uploads));
        }
        self.notice = "cleared finished".to_owned();
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        let len = self.list_len();
        let cursor = &mut self.cursors[self.view.index()];
        if len == 0 {
            *cursor = 0;
        } else if *cursor >= len {
            *cursor = len - 1;
        }
    }

    fn highlighted_transfer(&self) -> Option<(Direction, String, String)> {
        let row = match self.view {
            View::Dashboard => self.transfers.get(self.cursor()),
            View::Downloads => self
                .transfers
                .iter()
                .filter(|row| row.direction == Direction::Download)
                .nth(self.cursor()),
            View::Uploads => self
                .transfers
                .iter()
                .filter(|row| row.direction == Direction::Upload)
                .nth(self.cursor()),
            _ => None,
        }?;
        Some((row.direction, row.user.clone(), row.path.clone()))
    }

    fn list_len(&self) -> usize {
        match self.view {
            View::Dashboard => self.transfers.len(),
            View::Search => self.search_lines().len(),
            View::Downloads => self
                .transfers
                .iter()
                .filter(|row| row.direction == crate::model::Direction::Download)
                .count(),
            View::Uploads => self
                .transfers
                .iter()
                .filter(|row| row.direction == crate::model::Direction::Upload)
                .count(),
            View::Browse => self.browse.len(),
            View::Chat => self.rooms.len(),
            View::Users => self.users.len(),
            View::Shares => self.shares().len(),
            View::Settings => crate::settings::SECTIONS.len(),
            View::Library => self.column_len(self.library_column),
            View::Quality => self.quality_len(self.quality_column),
        }
    }
}

fn clamped_origin(origin: usize, len: usize, viewport: usize) -> usize {
    if viewport == 0 || len <= viewport {
        0
    } else {
        origin.min(len - viewport)
    }
}

fn step_origin(origin: usize, len: usize, viewport: usize, down: bool) -> usize {
    if viewport == 0 || len <= viewport {
        return 0;
    }
    let max = len - viewport;
    if down {
        origin.saturating_add(1).min(max)
    } else {
        origin.saturating_sub(1)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct SearchStamp {
    hits: u64,
    opens: u64,
    live: bool,
    query: String,
    exclude: String,
    include: String,
    country: String,
    quality: u8,
    min_bitrate: u32,
    min_duration: u32,
    min_size: u64,
    free_only: bool,
    file_type: u8,
}

fn push_history(history: &mut Vec<u64>, sample: u64) {
    history.push(sample);
    if history.len() > HISTORY {
        let extra = history.len() - HISTORY;
        history.drain(..extra);
    }
}

enum Flag {
    Notify,
    Priority,
    Trusted,
}

fn on_off(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerKind {
    Share,
    Up,
    Dir,
    File,
}

pub struct PickerLine {
    pub label: String,
    pub kind: PickerKind,
}

struct FolderPicker {
    cwd: PathBuf,
    rows: Vec<PickerRow>,
    cursor: usize,
}

struct BindPicker {
    rows: Vec<crate::settings::BindChoice>,
    cursor: usize,
}

#[derive(Clone)]
enum PickerRow {
    Share,
    Up,
    Dir(String),
    File(String),
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn read_folder(dir: &std::path::Path) -> Result<Vec<PickerRow>, String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|err| err.to_string())?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entry.path().is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    dirs.sort_by_key(|name| name.to_lowercase());
    files.sort_by_key(|name| name.to_lowercase());
    let mut rows = vec![PickerRow::Share, PickerRow::Up];
    rows.extend(dirs.into_iter().map(PickerRow::Dir));
    rows.extend(files.into_iter().map(PickerRow::File));
    Ok(rows)
}

fn path_list(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn join_paths(paths: &[String]) -> String {
    if paths.is_empty() {
        "none".to_owned()
    } else {
        paths.join(", ")
    }
}

fn share_counts(public: (u32, u32), buddy: (u32, u32), trusted: (u32, u32)) -> [ShareGroup; 3] {
    let mut groups = share_groups();
    groups[0].files = public.0;
    groups[0].folders = public.1;
    groups[1].files = buddy.0;
    groups[1].folders = buddy.1;
    groups[2].files = trusted.0;
    groups[2].folders = trusted.1;
    groups
}

const BITRATE_STEPS: [u32; 5] = [0, 128, 192, 256, 320];
const DURATION_STEPS: [u32; 4] = [0, 120, 240, 600];
const SIZE_STEPS: [u64; 5] = [0, 1_048_576, 5 * 1_048_576, 10 * 1_048_576, 50 * 1_048_576];
const FILE_TYPES: [&str; 8] = ["any", "flac", "mp3", "ogg", "opus", "aac", "wav", "ape"];

fn filter_row(name: &str, value: &str) -> String {
    format!("{name:<12}{value}")
}

fn blank(value: &str) -> &str {
    if value.trim().is_empty() {
        "—"
    } else {
        value.trim()
    }
}

fn quality_name(quality: u8) -> &'static str {
    match quality {
        1 => "lossless",
        2 => "lossy",
        _ => "any",
    }
}

fn bitrate_name(value: &u32) -> String {
    if *value == 0 {
        "any".to_owned()
    } else {
        format!(">={value}")
    }
}

fn duration_name(value: &u32) -> String {
    if *value == 0 {
        "any".to_owned()
    } else {
        format!(">={}:{:02}", value / 60, value % 60)
    }
}

fn size_name(value: &u64) -> String {
    if *value == 0 {
        "any".to_owned()
    } else {
        format!(">={} MB", value / 1_048_576)
    }
}

fn next_step<T: Copy + PartialEq>(current: T, steps: &[T]) -> T {
    let index = steps.iter().position(|step| *step == current).unwrap_or(0);
    steps[(index + 1) % steps.len()]
}

fn matches_type(hit: &SearchHit, kind: &str) -> bool {
    let Some(ext) = hit.extension() else {
        return false;
    };
    match kind {
        "aac" => {
            ext.eq_ignore_ascii_case("aac")
                || (ext.eq_ignore_ascii_case("m4a") && hit.format_rank() != 0)
        }
        "ogg" => ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("oga"),
        "wav" => ext.eq_ignore_ascii_case("wav") || ext.eq_ignore_ascii_case("wave"),
        other => ext.eq_ignore_ascii_case(other),
    }
}

fn file_count(count: usize) -> String {
    if count == 1 {
        "1 file will be deleted.".to_owned()
    } else {
        format!("{count} files will be deleted.")
    }
}

fn row_rank(album: bool, files: &[&SearchHit]) -> (bool, u32, u8, u8) {
    let queue = files.iter().map(|hit| hit.queue).min().unwrap_or(u32::MAX);
    let busy = u8::from(!files.iter().any(|hit| hit.free_slot));
    let format = files.iter().map(|hit| hit.format_rank()).min().unwrap_or(1);
    (!album, queue, busy, format)
}

struct AlbumRetry {
    album: String,
    exclude: String,
    armed: bool,
    token: Option<u32>,
    hits: Vec<SearchHit>,
    wait: u8,
}

struct FileRetryJob {
    file: String,
    exclude: String,
    path: String,
    folder: Option<String>,
}

struct FileRetry {
    file: String,
    exclude: String,
    path: String,
    folder: Option<String>,
    armed: bool,
    token: Option<u32>,
    hits: Vec<SearchHit>,
    wait: u8,
}

fn folder_album(folder: &str) -> &str {
    folder.rsplit(['\\', '/']).next().unwrap_or("").trim()
}

/// The free-slot copy of `album` with the highest reported upload speed.
fn choose_album_source(
    album: &str,
    exclude: &str,
    hits: &[SearchHit],
) -> Option<(String, String, Vec<SearchHit>)> {
    let mut groups: Vec<(String, String, Vec<SearchHit>)> = Vec::new();
    for hit in hits {
        if hit.user.eq_ignore_ascii_case(exclude) {
            continue;
        }
        let folder = parent_dir(&hit.path);
        if !folder_album(folder).eq_ignore_ascii_case(album) {
            continue;
        }
        if let Some(group) = groups
            .iter_mut()
            .find(|(user, dir, _)| user == &hit.user && dir == folder)
        {
            if !group.2.iter().any(|file| file.path == hit.path) {
                group.2.push(hit.clone());
            }
        } else {
            groups.push((hit.user.clone(), folder.to_owned(), vec![hit.clone()]));
        }
    }
    let mut best: Option<usize> = None;
    for (index, (_, _, files)) in groups.iter().enumerate() {
        if !files.iter().any(|hit| hit.free_slot) {
            continue;
        }
        let better = match best {
            None => true,
            Some(chosen) => album_source_rank(files) > album_source_rank(&groups[chosen].2),
        };
        if better {
            best = Some(index);
        }
    }
    best.map(|index| groups.swap_remove(index))
}

/// The free-slot copy of `file` with the highest reported upload speed.
fn choose_file_source<'a>(
    file: &str,
    exclude: &str,
    hits: &'a [SearchHit],
) -> Option<&'a SearchHit> {
    let mut best: Option<&SearchHit> = None;
    for hit in hits {
        if hit.user.eq_ignore_ascii_case(exclude) || !hit.free_slot {
            continue;
        }
        if !path_file(&hit.path).eq_ignore_ascii_case(file) {
            continue;
        }
        let better = match best {
            None => true,
            Some(chosen) => file_source_rank(hit) > file_source_rank(chosen),
        };
        if better {
            best = Some(hit);
        }
    }
    best
}

fn file_source_rank(hit: &SearchHit) -> (u32, u32) {
    (hit.upload_speed, u32::MAX - hit.queue)
}

fn path_file(path: &str) -> &str {
    path.rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(path)
}

fn album_source_rank(files: &[SearchHit]) -> (u32, u32, usize) {
    let speed = files.iter().map(|hit| hit.upload_speed).max().unwrap_or(0);
    let queue = files.iter().map(|hit| hit.queue).min().unwrap_or(u32::MAX);
    (speed, u32::MAX - queue, files.len())
}

fn folder_key(user: &str, folder: &str) -> String {
    format!("{user}\n{folder}")
}

fn parent_dir(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(0) | None => "",
        Some(index) => &path[..index],
    }
}

fn file_line(hit: &SearchHit, nested: bool) -> SearchLine {
    SearchLine {
        user: hit.user.clone(),
        path: hit.path.clone(),
        folder: false,
        expanded: false,
        nested,
        count: 1,
        size: hit.size,
        bitrate: hit.bitrate,
        duration: hit.duration,
        queue: hit.queue,
        free_slot: hit.free_slot,
        country: hit.country.clone(),
    }
}

fn folder_line(user: &str, folder: &str, files: &[&SearchHit], expanded: bool) -> SearchLine {
    let best = files[0];
    SearchLine {
        user: user.to_owned(),
        path: folder.to_owned(),
        folder: true,
        expanded,
        nested: false,
        count: u32::try_from(files.len()).unwrap_or(u32::MAX),
        size: files
            .iter()
            .fold(0u64, |sum, hit| sum.saturating_add(hit.size)),
        bitrate: best.bitrate,
        duration: files
            .iter()
            .filter_map(|hit| hit.duration)
            .reduce(u32::saturating_add),
        queue: files.iter().map(|hit| hit.queue).min().unwrap_or(0),
        free_slot: files.iter().any(|hit| hit.free_slot),
        country: best.country.clone(),
    }
}

fn listed(list: UserList, name: &str) -> ListedUser {
    ListedUser {
        list,
        name: name.to_owned(),
        status: UserStatus::Offline,
        country: "—".to_owned(),
        note: String::new(),
        notify: false,
        prioritized: false,
        trusted: false,
        last_seen: "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn hit(path: &str) -> SearchHit {
        SearchHit {
            user: "bob".to_owned(),
            path: path.to_owned(),
            size: 9,
            bitrate: None,
            duration: None,
            bit_depth: None,
            sample_rate: None,
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        }
    }

    fn choose(app: &mut App, label: &str) {
        let rows = app.picker_rows();
        let index = rows
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("missing {label}"));
        for _ in 0..index {
            app.on_key(key(KeyCode::Char('j')));
        }
        app.on_key(key(KeyCode::Enter));
    }

    #[test]
    fn listening_keeps_the_login_banner() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.apply_session(SessionEvent::Listening {
            port: 2234,
            address: "10.8.0.5".to_owned(),
        });
        assert_eq!(app.listen_ip(), Some("10.8.0.5"));
        assert_eq!(app.connection_label(), "alice · hello");
        assert!(app.logged_in());
        app.apply_session(SessionEvent::ListenFailed {
            message: "listen port 2234: address in use".to_owned(),
        });
        assert_eq!(app.connection_label(), "alice · hello");
        assert_eq!(app.notice(), "listen port 2234: address in use");
        assert_eq!(app.listen_ip(), None);
        app.apply_session(SessionEvent::Kicked);
        assert_eq!(app.connection_label(), "logged in elsewhere");
        assert!(!app.logged_in());
    }

    #[test]
    fn a_broken_pipe_stays_signed_in() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.apply_session(SessionEvent::Disconnected {
            message: "Broken pipe (os error 32). reconnecting".to_owned(),
        });
        assert!(app.logged_in());
        assert_eq!(app.connection_label(), "alice · hello");
        assert_eq!(app.notice(), "Broken pipe (os error 32). reconnecting");
    }

    #[test]
    fn keys_switch_views_and_quit() {
        let mut app = App::preview();
        app.on_key(key(KeyCode::Char('2')));
        assert_eq!(app.view(), View::Search);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor(), 1);
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.should_quit());
    }

    #[test]
    fn search_field_does_not_treat_digits_as_views() {
        let mut app = App::preview();
        app.on_key(key(KeyCode::Char('2')));
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('1')));
        app.on_key(key(KeyCode::Char('q')));
        assert_eq!(app.query(), "1q");
        assert_eq!(app.view(), View::Search);
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.notice(), "not logged in");
        assert!(!app.editing());
    }

    #[test]
    fn enter_queues_a_global_search_after_login() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.set_view(View::Search);
        app.on_key(key(KeyCode::Char('/')));
        for ch in ['j', 'a', 'z', 'z'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        let search = app.take_search().unwrap();
        assert_eq!(search.query, "jazz");
        assert_eq!(search.mode, SearchMode::Global);
        assert_eq!(app.notice(), "searching for jazz");
    }

    #[test]
    fn typing_green_day_sends_the_query_and_shows_a_result() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.set_view(View::Search);
        for ch in "green day".chars() {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        let search = app.take_search().unwrap();
        assert_eq!(search.query, "green day");
        assert_eq!(search.mode, SearchMode::Global);
        assert_eq!(app.notice(), "searching for green day");
        app.apply_session(SessionEvent::SearchBegan(1));
        app.apply_session(SessionEvent::SearchResult(
            1,
            SearchHit {
                user: "bob".to_owned(),
                path: "music\\basket-case.mp3".to_owned(),
                size: 9,
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
        assert!(
            app.visible_hits()
                .iter()
                .any(|hit| hit.path == "music\\basket-case.mp3")
        );
        assert_eq!(app.notice(), "1 result");
    }

    #[test]
    fn a_failed_peer_is_shown_in_the_notice() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        app.apply_session(SessionEvent::PeerFailed {
            username: "bob".to_owned(),
            message: "pierce connection failed".to_owned(),
        });
        assert_eq!(app.notice(), "bob: pierce connection failed");
        assert!(app.logged_in());
    }

    #[test]
    fn a_new_search_drops_green_day_results() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.set_view(View::Search);
        for ch in "green day".chars() {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        app.apply_session(SessionEvent::SearchBegan(1));
        for path in ["music\\Dookie\\01.flac", "music\\Dookie\\02.flac"] {
            app.apply_session(SessionEvent::SearchResult(1, hit(path)));
        }
        app.on_pointer(Target::Select(0));
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(
            app.take_folder(),
            Some(("bob".to_owned(), "music\\Dookie".to_owned()))
        );

        app.on_key(key(KeyCode::Char('/')));
        for _ in 0.."green day".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        for ch in "radiohead".chars() {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        app.apply_session(SessionEvent::SearchResult(1, hit("music\\Dookie\\03.flac")));
        assert!(app.visible_hits().is_empty());
        app.apply_session(SessionEvent::SearchBegan(2));
        app.apply_session(SessionEvent::SearchResult(1, hit("music\\Dookie\\04.flac")));
        app.apply_session(SessionEvent::Folder {
            user: "bob".to_owned(),
            directory: "music\\Dookie".to_owned(),
            files: vec![hit("music\\Dookie\\05.flac")],
        });
        app.apply_session(SessionEvent::SearchResult(
            2,
            hit("music\\OK Computer\\01.flac"),
        ));
        let paths: Vec<&str> = app
            .visible_hits()
            .iter()
            .map(|row| row.path.as_str())
            .collect();
        assert_eq!(paths, ["music\\OK Computer\\01.flac"]);
    }

    #[test]
    fn available_albums_lead_and_shorter_queues_lead_within_them() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        let rows = [
            ("amy", "music\\Slow\\a.flac", 8, true, None),
            ("amy", "music\\Slow\\b.flac", 8, true, None),
            ("bea", "music\\Fast\\a.mp3", 1, true, None),
            ("bea", "music\\Fast\\b.mp3", 1, true, None),
            ("cal", "music\\Now\\a.flac", 1, false, None),
            ("cal", "music\\Now\\b.flac", 1, false, None),
            ("gio", "music\\Lossless\\a.flac", 1, true, None),
            ("gio", "music\\Lossless\\b.mp3", 1, true, None),
            ("dee", "music\\dee.flac", 0, true, None),
            ("fay", "music\\fay.flac", 0, false, None),
            ("eve", "music\\eve.mp3", 3, true, None),
        ];
        for (user, path, queue, free_slot, bit_depth) in rows {
            app.apply_session(SessionEvent::SearchResult(
                0,
                SearchHit {
                    user: user.to_owned(),
                    path: path.to_owned(),
                    size: 9,
                    bitrate: None,
                    duration: None,
                    bit_depth,
                    sample_rate: None,
                    queue,
                    free_slot,
                    upload_speed: 0,
                    country: String::new(),
                },
            ));
        }
        let lines = app.search_lines();
        let paths: Vec<&str> = lines.iter().map(|line| line.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "music\\Lossless",
                "music\\Fast",
                "music\\Now",
                "music\\Slow",
                "music\\dee.flac",
                "music\\fay.flac",
                "music\\eve.mp3",
            ]
        );
    }

    #[test]
    fn lossless_results_stay_above_lossy() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        let rows = [
            ("music\\song.mp3", None),
            ("music\\cover.jpg", None),
            ("music\\album.flac", None),
            ("music\\track.wav", None),
            ("music\\radio.ogg", None),
            ("music\\disc.m4a", None),
            ("music\\apple.m4a", Some(16)),
            ("music\\disk.ape", None),
            ("music\\LIVE.FLAC", None),
        ];
        for (path, bit_depth) in rows {
            app.apply_session(SessionEvent::SearchResult(
                0,
                SearchHit {
                    user: "bob".to_owned(),
                    path: path.to_owned(),
                    size: 9,
                    bitrate: None,
                    duration: None,
                    bit_depth,
                    sample_rate: None,
                    queue: 0,
                    free_slot: true,
                    upload_speed: 0,
                    country: String::new(),
                },
            ));
        }
        let paths: Vec<&str> = app
            .visible_hits()
            .iter()
            .map(|hit| hit.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "music\\album.flac",
                "music\\track.wav",
                "music\\apple.m4a",
                "music\\disk.ape",
                "music\\LIVE.FLAC",
                "music\\cover.jpg",
                "music\\song.mp3",
                "music\\radio.ogg",
                "music\\disc.m4a",
            ]
        );
    }

    #[test]
    fn an_album_stays_one_row_until_opened() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.set_view(View::Search);
        let rows = [
            ("music\\Dookie\\basket.mp3", 3),
            ("music\\Dookie\\01.flac", 10),
            ("music\\Dookie\\02.wav", 8),
            ("music\\Other\\lone.mp3", 2),
        ];
        for (path, size) in rows {
            app.apply_session(SessionEvent::SearchResult(
                0,
                SearchHit {
                    user: "bob".to_owned(),
                    path: path.to_owned(),
                    size,
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
        }
        {
            let lines = app.search_lines();
            assert_eq!(lines.len(), 2);
            assert!(lines[0].folder);
            assert_eq!(lines[0].path, "music\\Dookie");
            assert_eq!(lines[0].count, 3);
            assert!(!lines[0].expanded);
            assert_eq!(lines[1].path, "music\\Other\\lone.mp3");
        }
        app.on_pointer(Target::Select(0));
        {
            let lines = app.search_lines();
            assert!(lines[0].expanded);
            let nested: Vec<&str> = lines
                .iter()
                .filter(|line| line.nested)
                .map(|line| line.path.as_str())
                .collect();
            assert_eq!(
                nested,
                [
                    "music\\Dookie\\01.flac",
                    "music\\Dookie\\02.wav",
                    "music\\Dookie\\basket.mp3",
                ]
            );
        }
        app.on_key(key(KeyCode::Char('d')));
        let mut queued = Vec::new();
        while let Some(download) = app.take_download() {
            assert_eq!(download.folder.as_deref(), Some("music\\Dookie"));
            queued.push(download.path);
        }
        assert_eq!(
            queued,
            [
                "music\\Dookie\\01.flac",
                "music\\Dookie\\02.wav",
                "music\\Dookie\\basket.mp3",
            ]
        );
        assert_eq!(
            app.take_folder(),
            Some(("bob".to_owned(), "music\\Dookie".to_owned()))
        );
        app.apply_session(SessionEvent::Folder {
            user: "bob".to_owned(),
            directory: "music\\Dookie".to_owned(),
            files: vec![SearchHit {
                user: "bob".to_owned(),
                path: "music\\Dookie\\03.flac".to_owned(),
                size: 11,
                bitrate: None,
                duration: None,
                bit_depth: Some(16),
                sample_rate: None,
                queue: 0,
                free_slot: true,
                upload_speed: 0,
                country: String::new(),
            }],
        });
        let extra = app.take_download().unwrap();
        assert_eq!(extra.path, "music\\Dookie\\03.flac");
        assert_eq!(extra.folder.as_deref(), Some("music\\Dookie"));
        assert!(
            app.search_lines()
                .iter()
                .any(|line| line.path.ends_with("03.flac"))
        );
    }

    #[test]
    fn dragging_across_an_album_leaves_it_open() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        app.set_view(View::Search);
        for path in ["music\\Dookie\\01.flac", "music\\Dookie\\02.flac"] {
            app.apply_session(SessionEvent::SearchResult(0, hit(path)));
        }
        app.on_pointer(Target::Select(0));
        assert!(app.search_lines()[0].expanded);
        assert_eq!(
            app.take_folder(),
            Some(("bob".to_owned(), "music\\Dookie".to_owned()))
        );
        app.select_row(0);
        assert!(app.search_lines()[0].expanded);
        assert!(app.take_folder().is_none());
        app.on_pointer(Target::Select(0));
        assert!(!app.search_lines()[0].expanded);
    }

    #[test]
    fn a_folder_reply_keeps_only_that_directory() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        app.set_view(View::Search);
        for path in ["music\\Dookie\\01.flac", "music\\Dookie\\02.flac"] {
            app.apply_session(SessionEvent::SearchResult(0, hit(path)));
        }
        app.on_pointer(Target::Select(0));
        let _ = app.take_folder();
        let mut files = vec![
            hit("music\\Dookie\\01.flac"),
            hit("music\\Dookie\\03.flac"),
            hit("music\\Dookie\\cd2\\04.flac"),
            hit("music\\Other\\note.txt"),
        ];
        files.extend((0..200).map(|index| hit(&format!("shares\\dump\\{index}.mp3"))));
        app.apply_session(SessionEvent::Folder {
            user: "bob".to_owned(),
            directory: "music\\Dookie".to_owned(),
            files,
        });
        let paths: Vec<&str> = app
            .visible_hits()
            .iter()
            .map(|row| row.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "music\\Dookie\\01.flac",
                "music\\Dookie\\02.flac",
                "music\\Dookie\\03.flac",
            ]
        );
    }

    #[test]
    fn search_lines_follow_a_filter_after_the_list_is_built() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        for path in [
            "music\\Dookie\\01.flac",
            "music\\Dookie\\02.flac",
            "music\\Nimrod\\01.flac",
            "music\\Nimrod\\02.flac",
        ] {
            app.apply_session(SessionEvent::SearchResult(0, hit(path)));
        }
        assert_eq!(app.search_lines().len(), 2);
        assert_eq!(app.search_lines().len(), 2);
        app.set_exclude("Nimrod");
        let lines = app.search_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].path, "music\\Dookie");
    }

    #[test]
    fn quality_filter_keeps_flac_and_drops_a_low_mp3() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.set_view(View::Search);
        for (path, bitrate, free_slot, country) in [
            ("music\\Dookie\\01.flac", None, true, "US"),
            ("music\\Dookie\\radio.mp3", Some(128), false, "DE"),
            ("music\\Dookie\\live.mp3", Some(320), true, "US"),
        ] {
            app.apply_session(SessionEvent::SearchResult(
                0,
                SearchHit {
                    user: "bob".to_owned(),
                    path: path.to_owned(),
                    size: 9,
                    bitrate,
                    duration: Some(200),
                    bit_depth: None,
                    sample_rate: None,
                    queue: 0,
                    free_slot,
                    upload_speed: 0,
                    country: country.to_owned(),
                },
            ));
        }
        assert_eq!(app.visible_hits().len(), 3);
        assert!(app.filter_lines()[0].ends_with("any"));
        app.on_key(key(KeyCode::Char(']')));
        assert!(app.filter_lines()[0].ends_with("lossless"));
        assert_eq!(
            app.visible_hits()
                .iter()
                .map(|hit| hit.path.as_str())
                .collect::<Vec<_>>(),
            ["music\\Dookie\\01.flac"]
        );
        app.on_pointer(Target::CycleFilter(1));
        for _ in 0..3 {
            app.on_pointer(Target::CycleFilter(1));
        }
        app.on_key(key(KeyCode::Char(']')));
        app.on_key(key(KeyCode::Char(']')));
        assert!(app.filter_lines()[1].ends_with(">=320"));
        let paths = app
            .visible_hits()
            .iter()
            .map(|hit| hit.path.clone())
            .collect::<Vec<_>>();
        assert!(paths.iter().any(|path| path.ends_with("01.flac")));
        assert!(paths.iter().any(|path| path.ends_with("live.mp3")));
        assert!(paths.iter().all(|path| !path.ends_with("radio.mp3")));
        app.on_pointer(Target::CycleFilter(4));
        assert!(app.filter_lines()[4].ends_with("flac"));
        assert_eq!(app.visible_hits().len(), 1);
        for _ in 0..7 {
            app.on_pointer(Target::CycleFilter(4));
        }
        app.on_pointer(Target::CycleFilter(1));
        app.on_pointer(Target::CycleFilter(7));
        for ch in ['d', 'o', 'o', 'k', 'i', 'e'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.filter_field().is_none());
        assert!(
            app.visible_hits()
                .iter()
                .all(|hit| hit.path.to_ascii_lowercase().contains("dookie"))
        );
        assert_eq!(app.visible_hits().len(), 3);
    }

    #[test]
    fn auto_away_waits_one_configured_minute() {
        let config = Config {
            auto_away_minutes: 1,
            ..Config::default()
        };
        let mut app = App::preview();
        app.bind_config(std::path::PathBuf::from("unused.toml"), config);
        for _ in 0..299 {
            app.on_tick();
        }
        assert_eq!(app.take_presence(), None);
        app.on_tick();
        assert_eq!(app.take_presence(), Some(true));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.take_presence(), Some(false));
    }

    #[test]
    fn picking_a_folder_saves_it_as_a_public_share() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-share-ui-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let music = dir.join("music");
        std::fs::create_dir_all(&music).unwrap();
        std::fs::write(music.join("song.txt"), b"hi").unwrap();
        let path = dir.join("config.toml");
        let mut app = App::preview();
        app.bind_config(path.clone(), Config::default());
        app.set_view(View::Shares);
        assert!(
            app.share_settings()
                .iter()
                .any(|(_, value)| { value == "shares view, enter picks a folder" })
        );
        app.open_folder_picker(dir.clone());
        assert!(app.picker_open());
        choose(&mut app, "music/");
        assert!(app.picker_rows().iter().any(|row| row.label == "song.txt"));
        let opened = app.picker_path();
        choose(&mut app, "song.txt");
        assert!(app.picker_open());
        assert_eq!(app.picker_path(), opened);
        app.on_key(key(KeyCode::Char('s')));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.shares.public, vec![music.clone()]);
        assert!(saved.shares.buddy.is_empty());
        let queued = app.take_rescan().unwrap();
        assert_eq!(queued.public, vec![music.clone()]);
        assert_eq!(app.notice(), "folder saved");
        assert!(!app.picker_open());

        app.on_key(key(KeyCode::Char('j')));
        let buddy = dir.join("buddy");
        std::fs::create_dir_all(&buddy).unwrap();
        app.open_folder_picker(dir.clone());
        choose(&mut app, "buddy/");
        app.on_key(key(KeyCode::Enter));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.shares.buddy, vec![buddy]);

        app.on_key(key(KeyCode::Char('k')));
        app.on_key(key(KeyCode::Char('x')));
        let saved = Config::load(&path).unwrap();
        assert!(saved.shares.public.is_empty());
        assert_eq!(app.notice(), "folder removed");
        assert!(app.take_rescan().unwrap().public.is_empty());

        app.open_folder_picker(dir.join("missing"));
        assert_eq!(app.notice(), "folder is missing");
        assert!(!app.picker_open());
        assert!(app.take_rescan().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_filed_download_folder_is_saved_as_a_public_share() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-share-root-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let library = dir.join("library");
        std::fs::create_dir_all(&library).unwrap();
        let path = dir.join("config.toml");
        let mut app = App::preview();
        app.bind_config(path.clone(), Config::default());
        app.apply_session(SessionEvent::ShareRoot {
            path: library.clone(),
        });
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.shares.public, vec![library.clone()]);
        assert!(saved.shares.buddy.is_empty());
        assert_eq!(app.take_rescan().unwrap().public, vec![library.clone()]);
        assert_eq!(app.notice(), "sharing the download folder");
        assert_eq!(
            app.share_paths()[0],
            &[library.to_string_lossy().into_owned()][..]
        );
        app.apply_session(SessionEvent::ShareRoot {
            path: library.clone(),
        });
        assert_eq!(Config::load(&path).unwrap().shares.public, vec![library]);
        assert!(app.take_rescan().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_screen_saves_upload_slots_and_a_speed_limit() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut app = App::preview();
        app.bind_config(path.clone(), Config::default());
        app.set_view(View::Settings);
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Backspace));
        app.on_key(key(KeyCode::Char('4')));
        app.on_key(key(KeyCode::Enter));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.upload_slots, 4);
        assert_eq!(app.notice(), "saved");
        assert_eq!(app.take_prefs().unwrap().upload_slots, 4);

        app.on_key(key(KeyCode::Char(']')));
        app.on_key(key(KeyCode::Char(']')));
        app.on_key(key(KeyCode::Enter));
        for ch in ['6', '4'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.upload_limit_kib, Some(64));
        app.on_key(key(KeyCode::Char('x')));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.upload_limit_kib, None);

        app.on_key(key(KeyCode::Char('k')));
        app.on_key(key(KeyCode::Char(']')));
        app.on_key(key(KeyCode::Enter));
        for _ in 0..4 {
            app.on_key(key(KeyCode::Backspace));
        }
        app.on_key(key(KeyCode::Char('0')));
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.notice(), "port must be 1–65535");
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.server_port, 2242);

        app.on_key(key(KeyCode::Char('j')));
        for _ in 0..4 {
            app.on_key(key(KeyCode::Char(']')));
        }
        app.on_key(key(KeyCode::Enter));
        app.open_folder_picker(dir.clone());
        app.on_key(key(KeyCode::Char('s')));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.incomplete_dir, dir.to_string_lossy());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn submitting_sign_in_saves_the_account() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-signin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut app = App::sign_in();
        app.bind_config(path.clone(), Config::default());
        assert_eq!(app.connection_label(), "sign in");
        for ch in ['a', 'd', 'a'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        for ch in ['s', 'e', 'c', 'r', 'e', 't'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Enter));
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.username, "ada");
        assert_eq!(saved.password(), "secret");
        let ready = app.take_sign_in().unwrap();
        assert_eq!(ready.username, "ada");
        assert_eq!(ready.password(), "secret");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_login_returns_to_the_form() {
        let mut app = App::connecting("ada");
        assert_eq!(app.connection_label(), "signing in");
        assert!(app.transfers().is_empty());
        app.apply_session(SessionEvent::LoginFailed {
            reason: "INVALIDPASS".to_owned(),
        });
        assert!(app.signing_in());
        assert_eq!(app.sign_username(), "ada");
        assert_eq!(app.notice(), "INVALIDPASS");
        assert_eq!(app.connection_label(), "INVALIDPASS");
    }

    #[test]
    fn login_clears_the_signing_in_notice() {
        let mut app = App::connecting("ada");
        assert_eq!(app.notice(), "signing in…");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        assert!(app.notice().is_empty());
        assert_eq!(app.connection_label(), "ada · hello");
    }

    #[test]
    fn live_meter_follows_transfer_byte_deltas() {
        let mut app = App::blank();
        app.on_tick();
        assert_eq!(*app.down_history().last().unwrap(), 0);
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        assert_eq!(app.feed(), Feed::Live);
        assert_eq!(*app.down_history().last().unwrap(), 0);
        let transfer = |done| Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "tone.bin".to_owned(),
            size: 10_000,
            done,
            speed: 0,
            queue: None,
            state: crate::model::TransferState::Transferring,
            detail: String::new(),
        };
        app.apply_session(SessionEvent::Transfer(transfer(0)));
        app.on_tick();
        app.apply_session(SessionEvent::Transfer(transfer(1_000)));
        app.on_tick();
        assert_eq!(*app.down_history().last().unwrap(), 5_000);
    }

    #[test]
    fn toggling_trusted_persists_across_load() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-trusted-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut config = Config::default();
        config.buddies.push(crate::config::Buddy {
            name: "ada".to_owned(),
            note: String::new(),
            notify: false,
            prioritized: false,
            trusted: false,
        });
        config.save(&path).unwrap();
        let mut app = App::preview();
        app.bind_config(path.clone(), Config::load(&path).unwrap());
        app.set_view(View::Users);
        let index = app
            .users()
            .iter()
            .position(|user| user.name == "ada")
            .unwrap();
        for _ in 0..index {
            app.on_key(key(KeyCode::Char('j')));
        }
        app.on_key(key(KeyCode::Char('t')));
        let loaded = Config::load(&path).unwrap();
        assert!(loaded.buddies[0].trusted);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn retry_hit(user: &str, path: &str, free_slot: bool, upload_speed: u32) -> SearchHit {
        SearchHit {
            user: user.to_owned(),
            path: path.to_owned(),
            size: 9,
            bitrate: None,
            duration: None,
            bit_depth: None,
            sample_rate: None,
            queue: 0,
            free_slot,
            upload_speed,
            country: String::new(),
        }
    }

    fn logged_in(app: &mut App) {
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: false,
        });
    }

    #[test]
    fn a_failed_album_downloads_from_the_fastest_free_user() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::AlbumFailed {
            user: "bob".to_owned(),
            album: "Dookie".to_owned(),
        });
        let search = app.take_search().unwrap();
        assert_eq!(search.query, "Dookie");
        assert_eq!(search.mode, SearchMode::Global);
        assert_eq!(app.notice(), "failed. searching for Dookie");
        app.apply_session(SessionEvent::SearchBegan(7));
        for (user, path, free_slot, speed) in [
            ("bob", "music\\Dookie\\01.flac", true, 900),
            ("carol", "music\\Dookie\\01.flac", true, 100),
            ("carol", "music\\Dookie\\02.flac", true, 100),
            ("dave", "music\\Dookie\\01.flac", true, 400),
            ("dave", "music\\Dookie\\02.flac", true, 400),
            ("eve", "music\\Dookie\\01.flac", false, 2_000),
            ("dave", "music\\Nimrod\\01.flac", true, 400),
        ] {
            app.apply_session(SessionEvent::SearchResult(
                7,
                retry_hit(user, path, free_slot, speed),
            ));
        }
        assert!(app.take_download().is_none());
        for _ in 0..ALBUM_RETRY_TICKS {
            app.on_tick();
        }
        let mut queued = Vec::new();
        while let Some(download) = app.take_download() {
            assert_eq!(download.user, "dave");
            assert_eq!(download.folder.as_deref(), Some("music\\Dookie"));
            queued.push(download.path);
        }
        queued.sort();
        assert_eq!(
            queued,
            [
                "music\\Dookie\\01.flac".to_owned(),
                "music\\Dookie\\02.flac".to_owned()
            ]
        );
        assert_eq!(
            app.take_folder(),
            Some(("dave".to_owned(), "music\\Dookie".to_owned()))
        );
        assert_eq!(app.notice(), "fetching 2 files of Dookie from dave");
    }

    #[test]
    fn a_failed_album_without_a_free_slot_is_not_queued() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::AlbumFailed {
            user: "bob".to_owned(),
            album: "Dookie".to_owned(),
        });
        let _ = app.take_search();
        app.apply_session(SessionEvent::SearchBegan(4));
        app.apply_session(SessionEvent::SearchResult(
            4,
            retry_hit("eve", "music\\Dookie\\01.flac", false, 2_000),
        ));
        for (path, state) in [
            ("music\\Dookie\\01.flac", TransferState::Failed),
            ("music\\Dookie\\02.flac", TransferState::Failed),
            ("music\\Nimrod\\01.flac", TransferState::Failed),
            ("tone.bin", TransferState::ConnectionClosed),
        ] {
            app.apply_session(SessionEvent::Transfer(Transfer {
                direction: Direction::Download,
                user: "bob".to_owned(),
                path: path.to_owned(),
                size: 10,
                done: 0,
                speed: 0,
                queue: None,
                state,
                detail: String::new(),
            }));
        }
        for _ in 0..ALBUM_RETRY_TICKS {
            app.on_tick();
        }
        assert!(app.take_download().is_none());
        assert_eq!(app.notice(), "Failed - Unavailable");
        let rows = app.transfers();
        let status = |path: &str| rows.iter().find(|row| row.path == path).unwrap().status();
        assert_eq!(status("music\\Dookie\\01.flac"), "Failed - Unavailable");
        assert_eq!(status("music\\Dookie\\02.flac"), "Failed - Unavailable");
        assert_eq!(status("music\\Nimrod\\01.flac"), "Failed");
        assert_eq!(status("tone.bin"), "Connection closed");
        assert_eq!(
            app.take_unavailable(),
            Some(("bob".to_owned(), "Dookie".to_owned()))
        );
    }

    #[test]
    fn a_file_not_shared_is_queued_from_the_fastest_free_user() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "music\\Dookie\\01.flac".to_owned(),
            size: 10,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::Failed,
            detail: String::new(),
        }));
        app.apply_session(SessionEvent::FileUnshared {
            user: "bob".to_owned(),
            file: "01.flac".to_owned(),
            path: "music\\Dookie\\01.flac".to_owned(),
            folder: Some("music\\Dookie".to_owned()),
        });
        let search = app.take_search().unwrap();
        assert_eq!(search.query, "01.flac");
        assert_eq!(search.mode, SearchMode::Global);
        assert_eq!(app.notice(), "failed. searching for 01.flac");
        app.apply_session(SessionEvent::SearchBegan(8));
        let mut queued_peer = retry_hit("dave", "music\\Other\\01.flac", true, 400);
        queued_peer.queue = 9;
        let mut open_peer = retry_hit("frank", "share\\01.FLAC", true, 400);
        open_peer.queue = 1;
        for hit in [
            retry_hit("bob", "music\\Dookie\\01.flac", true, 900),
            retry_hit("carol", "music\\Dookie\\02.flac", true, 2_000),
            retry_hit("eve", "live\\01.flac", false, 2_000),
            retry_hit("carol", "files\\01.flac", true, 100),
            queued_peer,
            open_peer,
        ] {
            app.apply_session(SessionEvent::SearchResult(8, hit));
        }
        assert!(app.take_download().is_none());
        for _ in 0..ALBUM_RETRY_TICKS {
            app.on_tick();
        }
        let download = app.take_download().unwrap();
        assert_eq!(download.user, "frank");
        assert_eq!(download.path, "share\\01.FLAC");
        assert_eq!(download.folder.as_deref(), Some("music\\Dookie"));
        assert!(app.take_download().is_none());
        assert_eq!(
            app.take_cancel(),
            Some((
                Direction::Download,
                "bob".to_owned(),
                "music\\Dookie\\01.flac".to_owned()
            ))
        );
        assert!(
            app.transfers()
                .iter()
                .all(|row| row.path != "music\\Dookie\\01.flac")
        );
        assert_eq!(app.notice(), "fetching 01.flac from frank");
    }

    #[test]
    fn a_file_not_shared_without_a_free_slot_is_not_queued() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "tone.bin".to_owned(),
            size: 8,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::Failed,
            detail: String::new(),
        }));
        app.apply_session(SessionEvent::FileUnshared {
            user: "bob".to_owned(),
            file: "tone.bin".to_owned(),
            path: "tone.bin".to_owned(),
            folder: None,
        });
        let _ = app.take_search();
        app.apply_session(SessionEvent::SearchBegan(5));
        app.apply_session(SessionEvent::SearchResult(
            5,
            retry_hit("eve", "elsewhere\\tone.bin", false, 2_000),
        ));
        for _ in 0..ALBUM_RETRY_TICKS {
            app.on_tick();
        }
        assert!(app.take_download().is_none());
        assert!(app.take_cancel().is_none());
        assert_eq!(app.notice(), "Failed - Unavailable");
        assert_eq!(app.transfers()[0].status(), "Failed - Unavailable");
        assert_eq!(
            app.take_file_unavailable(),
            Some(("bob".to_owned(), "tone.bin".to_owned()))
        );
    }

    #[test]
    fn d_queues_the_selected_search_hit() {
        let mut app = App::preview();
        app.set_account("alice");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::new(127, 0, 0, 1),
            supporter: true,
        });
        app.apply_session(SessionEvent::SearchResult(
            0,
            SearchHit {
                user: "bob".to_owned(),
                path: "music\\tone.bin".to_owned(),
                size: 65536,
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
        app.set_view(View::Search);
        app.set_exclude("preview");
        app.on_key(key(KeyCode::Char('d')));
        let download = app.take_download().unwrap();
        assert_eq!(download.user, "bob");
        assert_eq!(download.path, "music\\tone.bin");
        assert_eq!(download.size, 65536);
        assert!(download.folder.is_none());
    }

    #[test]
    fn exclude_token_hides_a_hit() {
        let mut app = App::preview();
        app.apply_session(SessionEvent::SearchResult(
            0,
            SearchHit {
                user: "bob".to_owned(),
                path: "jazz\\piano.mp3".to_owned(),
                size: 9,
                bitrate: None,
                duration: None,
                bit_depth: None,
                sample_rate: None,
                queue: 1,
                free_slot: true,
                upload_speed: 0,
                country: String::new(),
            },
        ));
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
    }

    #[test]
    fn the_library_hint_lists_the_playback_keys() {
        let mut app = App::preview();
        app.set_view(View::Library);
        assert_eq!(
            app.hints(),
            "enter plays   space pauses   s stops   x deletes"
        );
    }

    #[test]
    fn help_blocks_quit_until_it_closes() {
        let mut app = App::preview();
        app.on_key(key(KeyCode::Char('?')));
        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit());
        assert!(!app.help());
    }

    #[test]
    fn x_removes_the_highlighted_queue_row() {
        let mut app = App::preview();
        app.set_view(View::Downloads);
        assert!(app.hints().contains("x removes the row"));
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.notice(), "removed");
        assert!(app.take_cancel().is_none());
        assert!(
            app.transfers()
                .iter()
                .filter(|row| row.direction == Direction::Download)
                .all(|row| !row.path.contains("so-what"))
        );
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('x')));
        assert!(
            app.transfers()
                .iter()
                .all(|row| row.path != "preview/missing/side-a.mp3")
        );
        app.set_view(View::Uploads);
        app.on_key(key(KeyCode::Char('x')));
        assert!(
            app.transfers()
                .iter()
                .filter(|row| row.direction == Direction::Upload)
                .all(|row| !row.user.contains("chen"))
        );

        let mut live = App::preview();
        live.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        live.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "a.flac".to_owned(),
            size: 10,
            done: 0,
            speed: 0,
            queue: Some(3),
            state: crate::model::TransferState::Queued,
            detail: String::new(),
        }));
        live.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "b.flac".to_owned(),
            size: 10,
            done: 0,
            speed: 0,
            queue: Some(4),
            state: crate::model::TransferState::Queued,
            detail: String::new(),
        }));
        live.set_view(View::Downloads);
        live.on_key(key(KeyCode::Char('x')));
        assert_eq!(
            live.take_cancel(),
            Some((Direction::Download, "bob".to_owned(), "a.flac".to_owned()))
        );
        assert!(live.transfers().iter().all(|row| row.path != "a.flac"));
        assert!(live.transfers().iter().any(|row| row.path == "b.flac"));
    }

    #[test]
    fn x_clears_finished_rows_and_leaves_the_rest() {
        let mut app = App::connecting("ada");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "done.flac".to_owned(),
            size: 10,
            done: 10,
            speed: 0,
            queue: None,
            state: TransferState::Finished,
            detail: String::new(),
        }));
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: "wait.flac".to_owned(),
            size: 10,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::Queued,
            detail: String::new(),
        }));
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Upload,
            user: "cara".to_owned(),
            path: "sent.flac".to_owned(),
            size: 10,
            done: 10,
            speed: 0,
            queue: None,
            state: TransferState::Finished,
            detail: String::new(),
        }));
        app.set_view(View::Downloads);
        app.on_key(key(KeyCode::Char('X')));
        assert_eq!(app.notice(), "cleared finished");
        assert_eq!(app.take_clear_finished(), Some((true, false)));
        assert!(app.transfers().iter().all(|row| row.path != "done.flac"));
        assert!(app.transfers().iter().any(|row| row.path == "wait.flac"));
        assert!(app.transfers().iter().any(|row| row.path == "sent.flac"));
    }

    #[test]
    fn clicking_a_room_member_opens_a_card_that_browses_and_adds_a_friend() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-friend-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut config = Config::default();
        config.save(&path).unwrap();
        let mut app = App::preview();
        app.bind_config(path.clone(), Config::load(&path).unwrap());
        logged_in(&mut app);
        app.apply_session(SessionEvent::RoomMembers {
            room: "jazz".to_owned(),
            members: vec![crate::model::RoomPerson {
                name: "bob".to_owned(),
                status: Some(2),
                country: "US".to_owned(),
                files: Some(12),
                dirs: Some(3),
            }],
        });
        app.set_view(View::Chat);
        app.on_pointer(Target::Person(0));
        assert!(app.person_open());
        assert_eq!(app.person_name(), Some("bob"));
        assert_eq!(app.member_cursor(), 0);
        assert_eq!(app.take_inspect().as_deref(), Some("bob"));
        assert!(
            app.person_summary()
                .iter()
                .any(|line| line.contains("online"))
        );
        assert!(
            app.person_summary()
                .iter()
                .any(|line| line.contains("12 files"))
        );
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.take_browse().as_deref(), Some("bob"));
        assert_eq!(app.view(), View::Browse);
        assert!(!app.person_open());

        app.set_view(View::Chat);
        app.on_pointer(Target::Person(0));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Enter));
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.buddies[0].name, "bob");
        assert!(app.users().iter().any(|user| user.name == "bob"));
        assert_eq!(app.notice(), "added bob");
        assert!(app.person_open());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn typing_in_a_room_shows_the_draft_and_enter_sends_it() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::RoomListed {
            room: "jazz".to_owned(),
            users: 4,
        });
        app.apply_session(SessionEvent::RoomListed {
            room: "electronic".to_owned(),
            users: 9,
        });
        app.set_view(View::Chat);
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.cursor(), 1);
        for ch in ['h', 'e', 'l', 'l', 'o'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(app.view(), View::Chat);
        assert_eq!(app.cursor(), 1);
        assert!(app.saying());
        assert_eq!(app.say_draft(), "hello");
        assert!(!app.should_quit());
        app.on_key(key(KeyCode::Char('q')));
        assert!(!app.should_quit());
        assert_eq!(app.say_draft(), "helloq");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.say_draft(), "hello");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.take_say(),
            Some(("jazz".to_owned(), "hello".to_owned()))
        );
        assert!(app.say_draft().is_empty());
        assert!(!app.saying());
        assert_eq!(app.view(), View::Chat);
    }

    #[test]
    fn rooms_are_ordered_by_size_then_name() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::RoomListed {
            room: "jazz".to_owned(),
            users: 9,
        });
        app.apply_session(SessionEvent::RoomListed {
            room: "electronic".to_owned(),
            users: 9,
        });
        app.apply_session(SessionEvent::RoomListed {
            room: "ambient".to_owned(),
            users: 4,
        });
        let names: Vec<&str> = app.rooms().iter().map(|room| room.name.as_str()).collect();
        assert_eq!(names, ["electronic", "jazz", "ambient"]);
        app.set_view(View::Chat);
        app.on_pointer(Target::Select(1));
        assert_eq!(app.rooms()[app.cursor()].name, "jazz");
        app.apply_session(SessionEvent::RoomListed {
            room: "ambient".to_owned(),
            users: 20,
        });
        let names: Vec<&str> = app.rooms().iter().map(|room| room.name.as_str()).collect();
        assert_eq!(names, ["ambient", "electronic", "jazz"]);
        assert_eq!(app.rooms()[app.cursor()].name, "jazz");
    }

    #[test]
    fn members_are_listed_alphabetically() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::RoomListed {
            room: "jazz".to_owned(),
            users: 3,
        });
        app.set_view(View::Chat);
        app.apply_session(SessionEvent::RoomMembers {
            room: "jazz".to_owned(),
            members: vec![
                RoomPerson::named("zoe"),
                RoomPerson::named("amy"),
                RoomPerson::named("mia"),
            ],
        });
        let names: Vec<String> = app
            .room_members()
            .into_iter()
            .map(|person| person.name)
            .collect();
        assert_eq!(names, ["amy", "mia", "zoe"]);
        app.on_key(key(KeyCode::Char(']')));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.room_members()[app.member_cursor()].name, "mia");
        app.apply_session(SessionEvent::RoomMembers {
            room: "jazz".to_owned(),
            members: vec![
                RoomPerson::named("zoe"),
                RoomPerson::named("bea"),
                RoomPerson::named("amy"),
                RoomPerson::named("mia"),
            ],
        });
        let names: Vec<String> = app
            .room_members()
            .into_iter()
            .map(|person| person.name)
            .collect();
        assert_eq!(names, ["amy", "bea", "mia", "zoe"]);
        assert_eq!(app.room_members()[app.member_cursor()].name, "mia");
    }

    #[test]
    fn clicking_a_listed_room_asks_to_join_it() {
        let mut app = App::preview();
        logged_in(&mut app);
        app.apply_session(SessionEvent::RoomListed {
            room: "jazz".to_owned(),
            users: 40,
        });
        app.set_view(View::Chat);
        app.on_pointer(Target::Select(0));
        assert_eq!(app.take_join().as_deref(), Some("jazz"));
        assert_eq!(app.cursor(), 0);
    }

    #[test]
    fn pointer_selects_scrolls_and_help_eats_the_next_click() {
        let mut app = App::preview();
        app.on_pointer(Target::OpenView(View::Search));
        assert_eq!(app.view(), View::Search);
        app.on_pointer(Target::FocusQuery);
        assert!(app.editing());
        app.on_pointer(Target::CycleMode);
        assert_eq!(app.mode(), SearchMode::Room);
        assert!(app.editing());
        app.on_pointer(Target::Select(1));
        assert_eq!(app.cursor(), 1);
        assert!(!app.editing());
        app.on_scroll(true);
        assert_eq!(app.cursor(), 1);
        app.on_pointer(Target::OpenHelp);
        assert!(app.help());
        app.on_scroll(true);
        assert_eq!(app.cursor(), 1);
        app.on_pointer(Target::Select(0));
        assert!(!app.help());
        assert_eq!(app.cursor(), 1);
    }

    #[test]
    fn mode_cycles_and_query_filters_the_preview_catalog() {
        let mut app = App::preview();
        app.set_view(View::Search);
        app.on_key(key(KeyCode::Char('m')));
        assert_eq!(app.mode(), SearchMode::Room);
        app.on_key(key(KeyCode::Char('/')));
        for ch in ['x', 't', 'a', 'l'] {
            app.on_key(key(KeyCode::Char(ch)));
        }
        app.on_key(key(KeyCode::Esc));
        let hits = app.visible_hits();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.contains("xtal"));
    }

    fn download_row(index: usize) -> Transfer {
        Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: format!("file-{index:03}.bin"),
            size: 10,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::Queued,
            detail: String::new(),
        }
    }

    #[test]
    fn twenty_steps_follow_the_bottom_edge() {
        let mut app = App::blank();
        app.set_view(View::Downloads);
        app.transfers = (0..100).map(download_row).collect();
        for _ in 0..20 {
            app.on_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.cursor(), 20);
        assert_eq!(app.window_origin(ScrollId::List, 100, 10), 11);
    }

    #[test]
    fn dragging_the_thumb_leaves_the_selection() {
        let mut app = App::blank();
        app.set_view(View::Downloads);
        app.transfers = (0..100).map(download_row).collect();
        let metrics = crate::ui::pointer::ScrollMetrics {
            len: 100,
            viewport: 10,
            origin: 0,
            track: ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 10,
            },
        };
        app.begin_scroll(ScrollId::List, 0, metrics);
        assert!(app.drag_scroll(4));
        app.end_scroll();
        assert!(!app.drag_scroll(8));
        assert_eq!(app.window_origin(ScrollId::List, 100, 10), 40);
        assert_eq!(app.cursor(), 0);
        app.scroll_by(ScrollId::List, true, Some(metrics));
        assert_eq!(app.cursor(), 0);
        assert_eq!(app.window_origin(ScrollId::List, 100, 10), 41);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.cursor(), 1);
        assert_eq!(app.window_origin(ScrollId::List, 100, 10), 1);
    }

    #[test]
    fn transcript_stays_pinned_until_scrolled_up() {
        let mut app = App::blank();
        assert_eq!(app.window_origin(ScrollId::Transcript, 50, 10), 40);
        assert_eq!(app.window_origin(ScrollId::Transcript, 80, 10), 70);
        let metrics = crate::ui::pointer::ScrollMetrics {
            len: 50,
            viewport: 10,
            origin: 40,
            track: ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 10,
            },
        };
        app.scroll_by(ScrollId::Transcript, false, Some(metrics));
        assert_eq!(app.window_origin(ScrollId::Transcript, 50, 10), 39);
        assert_eq!(app.window_origin(ScrollId::Transcript, 80, 10), 39);
        app.apply_session(SessionEvent::Chat(ChatLine {
            room: "hall".to_owned(),
            time: "00:00".to_owned(),
            user: "ada".to_owned(),
            text: "later".to_owned(),
        }));
        assert_eq!(app.window_origin(ScrollId::Transcript, 80, 10), 39);
    }

    #[test]
    fn quality_analysis_runs_on_its_own_thread() {
        crate::quality::pause_workers(true);
        let root = std::env::temp_dir().join(format!(
            "soul-sever-qthread-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let song_path = root.join("song.wav");
        crate::quality::write_wav(
            &song_path,
            44_100,
            16,
            &crate::quality::tone(44_100, 20_000.0, 44_100 * 2),
        );
        let config = crate::config::Config {
            download_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut app = App::preview();
        app.bind_config(root.join("config.toml"), config);
        let started = std::time::Instant::now();
        while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
            app.poll_library();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.library_scan_pending());
        app.poll_quality();
        let started = std::time::Instant::now();
        while crate::quality::worker_thread_name() != "quality"
            && started.elapsed() < std::time::Duration::from_secs(2)
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(crate::quality::worker_thread_name(), "quality");
        assert_ne!(std::thread::current().name(), Some("quality"));
        assert!(
            app.track_rows().iter().all(|row| !row.contains('%')),
            "{:?}",
            app.track_rows()
        );
        let song = app.selected_song().unwrap().clone();
        app.player.play(&song);
        let started = std::time::Instant::now();
        while !app.player.is_playing() && started.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.player.is_playing());
        assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
        app.poll_quality();
        assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
        crate::quality::pause_workers(false);
        let started = std::time::Instant::now();
        while app.track_rows().iter().all(|row| !row.contains('%'))
            && started.elapsed() < std::time::Duration::from_secs(5)
        {
            app.poll_quality();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            app.track_rows().iter().any(|row| row.contains('%')),
            "{:?}",
            app.track_rows()
        );
        app.player.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn r_checks_the_library_again() {
        struct ReleaseWorkers;
        impl Drop for ReleaseWorkers {
            fn drop(&mut self) {
                crate::quality::pause_workers(false);
            }
        }
        let _release = ReleaseWorkers;
        crate::quality::pause_workers(true);
        let root = std::env::temp_dir().join(format!(
            "soul-sever-recheck-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let song_path = root.join("Again.wav");
        crate::quality::write_wav(
            &song_path,
            44_100,
            16,
            &crate::quality::tone(44_100, 1_000.0, 8_000),
        );
        let config = crate::config::Config {
            download_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut app = App::preview();
        app.bind_config(root.join("config.toml"), config);
        let started = std::time::Instant::now();
        while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
            app.poll_library();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.library_scan_pending());
        app.poll_quality();
        let path = app
            .catalog
            .artists
            .iter()
            .flat_map(|artist| artist.albums.iter())
            .flat_map(|album| album.songs.iter())
            .find(|song| song.title == "Again")
            .unwrap()
            .path
            .clone();
        app.catalog.set_quality(
            &path,
            crate::quality::Quality {
                bit_depth: 16,
                sample_rate: 44_100,
                lossy_percent: Some(99),
                cutoff_hz: 1_000,
                duration_secs: Some(1),
                verdict: crate::quality::Verdict::Lossy,
                upgrade_attempted: true,
            },
        );
        app.set_view(View::Library);
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.catalog.quality(&path).unwrap().lossy_percent, Some(99));
        app.set_view(View::Quality);
        app.on_key(key(KeyCode::Char('r')));
        assert_eq!(app.notice(), "rechecking quality");
        assert!(app.catalog.quality(&path).is_none());
        assert!(app.quality_len(0) >= 1, "measuring {}", app.quality_len(0));
        assert!(
            app.quality_label(0, 0).contains("Again"),
            "{}",
            app.quality_label(0, 0)
        );
        app.poll_quality();
        assert!(app.catalog.quality(&path).is_none());
        crate::quality::pause_workers(false);
        let started = std::time::Instant::now();
        while app.catalog.quality(&path).is_none()
            && started.elapsed() < std::time::Duration::from_secs(5)
        {
            app.poll_quality();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let quality = app.catalog.quality(&path).unwrap().clone();
        assert!(!quality.upgrade_attempted);
        assert_ne!(quality.lossy_percent, Some(99));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_quality_hit_stays_off_the_search_list() {
        let mut app = App::preview();
        app.set_account("alice");
        app.hits.push(SearchHit {
            user: "ada".to_owned(),
            path: "music\\shown.flac".to_owned(),
            size: 10,
            bitrate: None,
            duration: None,
            bit_depth: Some(16),
            sample_rate: Some(44_100),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        });
        let shown = app.hits.clone();
        app.quality_probe = Some(QualityProbe {
            path: PathBuf::from("/music/song.wav"),
            artist: "Ada".to_owned(),
            title: "Song".to_owned(),
            duration: Some(120),
            goal: crate::quality::UpgradeGoal::AnyLossless,
            hits: Vec::new(),
            ticks: 0,
        });
        app.apply_session(SessionEvent::QualityHit(SearchHit {
            user: "bob".to_owned(),
            path: "music\\Song.flac".to_owned(),
            size: 20,
            bitrate: None,
            duration: Some(120),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        }));
        assert_eq!(app.hits, shown);
        assert_eq!(app.quality_probe.as_ref().unwrap().hits.len(), 1);
        assert_eq!(
            app.quality_probe.as_ref().unwrap().hits[0].sample_rate,
            Some(96_000)
        );
    }

    #[test]
    fn the_quality_view_shows_measurement_and_the_replacement() {
        crate::quality::pause_workers(true);
        let root = std::env::temp_dir().join(format!(
            "soul-sever-qview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        crate::quality::write_wav(
            &root.join("Narrow.wav"),
            44_100,
            16,
            &crate::quality::tone(44_100, 1_000.0, 8_000),
        );
        crate::quality::write_wav(
            &root.join("Wide.wav"),
            44_100,
            16,
            &crate::quality::tone(44_100, 1_000.0, 8_000),
        );
        let config = crate::config::Config {
            download_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut app = App::preview();
        app.bind_config(root.join("config.toml"), config);
        let started = std::time::Instant::now();
        while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
            app.poll_library();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.library_scan_pending());
        let wide = app
            .catalog
            .artists
            .iter()
            .flat_map(|artist| artist.albums.iter())
            .flat_map(|album| album.songs.iter())
            .find(|song| song.title == "Wide")
            .unwrap()
            .path
            .clone();
        app.catalog.set_quality(
            &wide,
            crate::quality::Quality {
                bit_depth: 16,
                sample_rate: 44_100,
                lossy_percent: Some(27),
                cutoff_hz: 16_000,
                duration_secs: Some(1),
                verdict: crate::quality::Verdict::Lossy,
                upgrade_attempted: false,
            },
        );
        app.set_account("ada");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        let shown = app.hits.clone();
        app.poll_quality();
        app.apply_session(SessionEvent::QualityHit(SearchHit {
            user: "bob".to_owned(),
            path: "Unknown Artist\\Wide.flac".to_owned(),
            size: 20,
            bitrate: None,
            duration: Some(1),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        }));
        app.apply_session(SessionEvent::QualityHit(SearchHit {
            user: "cara".to_owned(),
            path: "Unknown Artist\\Wide-live.flac".to_owned(),
            size: 18,
            bitrate: None,
            duration: Some(1),
            bit_depth: Some(16),
            sample_rate: Some(44_100),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        }));
        app.set_view(View::Quality);
        app.focus_quality(1, 0);
        assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
        assert_eq!(app.hits, shown);
        let during = render_quality(&app);
        assert!(during.contains("measuring"), "{during}");
        assert!(during.contains("Narrow"), "{during}");
        assert!(during.contains("replacing"), "{during}");
        assert!(during.contains("Wide  needs a lossless copy"), "{during}");
        assert!(during.contains("looking now"), "{during}");
        assert!(
            during.contains("have 16-bit · 44.1 kHz · lossy · 27%"),
            "{during}"
        );
        assert!(!during.contains("searching"), "{during}");
        assert!(!during.contains("bob"), "{during}");
        assert!(!during.contains("cara"), "{during}");
        assert!(!during.contains("found"), "{during}");
        for _ in 0..40 {
            app.on_tick();
        }
        app.focus_quality(1, 0);
        let replacing = render_quality(&app);
        crate::quality::pause_workers(false);
        assert!(replacing.contains("replacing"), "{replacing}");
        assert!(replacing.contains("Narrow"), "{replacing}");
        assert!(
            replacing.contains("Wide  24-bit · 96 kHz · starting the download"),
            "{replacing}"
        );
        assert!(replacing.contains("from bob · Wide.flac"), "{replacing}");
        assert!(!replacing.contains("cara"), "{replacing}");
        assert!(!replacing.contains("found"), "{replacing}");
        assert!(!replacing.contains("offered"), "{replacing}");
        assert_eq!(app.hits, shown);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn replacements_from_different_users_download_together() {
        crate::quality::pause_workers(true);
        let root = std::env::temp_dir().join(format!(
            "soul-sever-qparallel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for title in ["Narrow", "Wide", "Tiny"] {
            crate::quality::write_wav(
                &root.join(format!("{title}.wav")),
                44_100,
                16,
                &crate::quality::tone(44_100, 1_000.0, 8_000),
            );
        }
        let config = crate::config::Config {
            download_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut app = App::preview();
        app.bind_config(root.join("config.toml"), config);
        let started = std::time::Instant::now();
        while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
            app.poll_library();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!app.library_scan_pending());
        for title in ["Narrow", "Wide", "Tiny"] {
            let path = app
                .catalog
                .entries()
                .find(|(song, _)| song.title == title)
                .unwrap()
                .0
                .path
                .clone();
            app.catalog.set_quality(
                &path,
                crate::quality::Quality {
                    bit_depth: 16,
                    sample_rate: 44_100,
                    lossy_percent: Some(27),
                    cutoff_hz: 16_000,
                    duration_secs: Some(1),
                    verdict: crate::quality::Verdict::Lossy,
                    upgrade_attempted: false,
                },
            );
        }
        app.set_account("ada");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        let hit = |user: &str, title: &str, depth: u32, rate: u32| SearchHit {
            user: user.to_owned(),
            path: format!("Unknown Artist\\{title}.flac"),
            size: 20,
            bitrate: None,
            duration: Some(1),
            bit_depth: Some(depth),
            sample_rate: Some(rate),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        };
        let finish = |app: &mut App| {
            for _ in 0..40 {
                app.on_tick();
            }
        };
        app.poll_quality();
        let first = app.quality_probe.as_ref().unwrap().title.clone();
        app.apply_session(SessionEvent::QualityHit(hit("bob", &first, 24, 96_000)));
        finish(&mut app);
        let first_download = app.take_download().unwrap();
        let second = app.quality_probe.as_ref().unwrap().title.clone();
        app.apply_session(SessionEvent::QualityHit(hit("bob", &second, 24, 96_000)));
        app.apply_session(SessionEvent::QualityHit(hit("cara", &second, 16, 44_100)));
        finish(&mut app);
        let second_download = app.take_download().unwrap();
        let none_yet = app.take_download();
        let third = app.quality_probe.as_ref().unwrap().title.clone();
        app.apply_session(SessionEvent::QualityHit(hit("bob", &third, 24, 96_000)));
        finish(&mut app);
        let held = app.take_download();
        app.set_view(View::Quality);
        app.focus_quality(1, 2);
        let text = render_quality(&app);
        let third_attempted = app
            .catalog
            .entries()
            .find(|(song, _)| song.title == third)
            .and_then(|(_, quality)| quality)
            .is_some_and(|quality| quality.upgrade_attempted);
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: first_download.user.clone(),
            path: first_download.path.clone(),
            size: first_download.size,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::FileNotShared,
            detail: String::new(),
        }));
        let resumed = app.take_download();
        crate::quality::pause_workers(false);
        assert_eq!(first_download.user, "bob");
        assert_ne!(first, second);
        assert_eq!(second_download.user, "cara");
        assert!(
            second_download.path.contains(&second),
            "{}",
            second_download.path
        );
        assert!(none_yet.is_none());
        assert!(held.is_none());
        assert!(!third_attempted);
        assert!(text.contains(&first), "{text}");
        assert!(text.contains(&second), "{text}");
        assert!(text.contains("waiting for a free user"), "{text}");
        assert!(!text.contains("found"), "{text}");
        let resumed = resumed.unwrap();
        assert_eq!(resumed.user, "bob");
        assert!(resumed.path.contains(&third), "{}", resumed.path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_full_queue_tries_the_next_replacement() {
        crate::quality::pause_workers(true);
        let root = std::env::temp_dir().join(format!(
            "soul-sever-qnext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        crate::quality::write_wav(
            &root.join("Wide.wav"),
            44_100,
            16,
            &crate::quality::tone(44_100, 1_000.0, 8_000),
        );
        let config = crate::config::Config {
            download_dir: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let mut app = App::preview();
        app.bind_config(root.join("config.toml"), config);
        let started = std::time::Instant::now();
        while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
            app.poll_library();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let path = app
            .catalog
            .entries()
            .find(|(song, _)| song.title == "Wide")
            .unwrap()
            .0
            .path
            .clone();
        app.catalog.set_quality(
            &path,
            crate::quality::Quality {
                bit_depth: 16,
                sample_rate: 44_100,
                lossy_percent: Some(27),
                cutoff_hz: 16_000,
                duration_secs: Some(1),
                verdict: crate::quality::Verdict::Lossy,
                upgrade_attempted: false,
            },
        );
        app.set_account("ada");
        app.apply_session(SessionEvent::LoggedIn {
            banner: "hello".to_owned(),
            ip: std::net::Ipv4Addr::LOCALHOST,
            supporter: false,
        });
        let hit = |user: &str, depth: u32, rate: u32| SearchHit {
            user: user.to_owned(),
            path: format!("Unknown Artist\\{user}-Wide.flac"),
            size: 20,
            bitrate: None,
            duration: Some(1),
            bit_depth: Some(depth),
            sample_rate: Some(rate),
            queue: 0,
            free_slot: true,
            upload_speed: 0,
            country: String::new(),
        };
        app.poll_quality();
        app.apply_session(SessionEvent::QualityHit(hit("bob", 24, 96_000)));
        app.apply_session(SessionEvent::QualityHit(hit("cara", 16, 44_100)));
        for _ in 0..40 {
            app.on_tick();
        }
        let first = app.take_download().unwrap();
        app.apply_session(SessionEvent::Transfer(Transfer {
            direction: Direction::Download,
            user: first.user.clone(),
            path: first.path.clone(),
            size: first.size,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::InternalError,
            detail: String::new(),
        }));
        let next = app.take_download();
        let cancel = app.take_cancel();
        let attempted = app
            .catalog
            .quality(&path)
            .is_some_and(|quality| quality.upgrade_attempted);
        crate::quality::pause_workers(false);
        assert_eq!(first.user, "bob");
        let next = next.unwrap();
        assert_eq!(next.user, "cara");
        assert!(next.stage);
        assert_eq!(
            cancel,
            Some((Direction::Download, "bob".to_owned(), first.path))
        );
        assert!(!attempted);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn render_quality(app: &App) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut pointer = crate::ui::Pointer::default();
        terminal
            .draw(|frame| crate::ui::draw(frame, app, &mut pointer))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        crate::ui::screen_text(120, 40, &buffer)
    }
}
