use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use std::net::Ipv4Addr;

use soul_sever::model::{
    ChatLine, Direction, RoomPerson, SearchHit, SearchMode, Transfer, TransferState,
};
use soul_sever::session::SessionEvent;
use soul_sever::ui::{self, Pointer, screen_text};
use soul_sever::{App, Edge, ScrollId, Split, Target, View};

fn render(app: &App, width: u16, height: u16) -> String {
    render_pointer(app, width, height).0
}

fn render_pointer(app: &App, width: u16, height: u16) -> (String, Pointer) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut pointer = Pointer::default();
    terminal
        .draw(|frame| ui::draw(frame, app, &mut pointer))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (screen_text(width, height, &buffer), pointer)
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn the_bind_address_is_chosen_from_the_interfaces() {
    let dir = std::env::temp_dir().join(format!(
        "soul-sever-bind-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    let mut app = App::preview();
    app.bind_config(path.clone(), soul_sever::config::Config::default());
    press(&mut app, KeyCode::Char('9'));
    press(&mut app, KeyCode::Char('j'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Char(']'));
    }
    press(&mut app, KeyCode::Enter);
    let opened = render(&app, 120, 40);
    assert!(opened.contains("bind address"), "{opened}");
    assert!(opened.contains("all interfaces"), "{opened}");
    assert!(opened.contains("127.0.0.1"), "{opened}");
    let index = app
        .bind_rows()
        .iter()
        .position(|row| row.contains("127.0.0.1"))
        .expect("loopback");
    let (_text, pointer) = render_pointer(&app, 120, 40);
    let row = pointer
        .region(|target| target == Target::Picker(index))
        .expect("interface row");
    app.on_pointer(pointer.at(row.x, row.y).unwrap());
    assert!(!app.bind_picker_open());
    assert_eq!(app.notice(), "saved. restart to apply");
    let saved = soul_sever::config::Config::load(&path).unwrap();
    assert_eq!(saved.bind_address, "127.0.0.1");
    let shown = render(&app, 120, 40);
    assert!(shown.contains("127.0.0.1"), "{shown}");
    assert!(!shown.contains("all interfaces"), "{shown}");

    press(&mut app, KeyCode::Enter);
    assert!(app.bind_picker_open());
    press(&mut app, KeyCode::Esc);
    assert!(!app.bind_picker_open());
    assert_eq!(
        soul_sever::config::Config::load(&path)
            .unwrap()
            .bind_address,
        "127.0.0.1"
    );
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        soul_sever::config::Config::load(&path)
            .unwrap()
            .bind_address,
        ""
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_log_shows_a_browse_and_collapses_to_the_latest_line() {
    let mut app = App::preview();
    app.apply_session(SessionEvent::Log(
        "User bob is browsing your list of shared files".to_owned(),
    ));
    app.apply_session(SessionEvent::Log(
        "User bob is searching for \"notes\", found 1 results".to_owned(),
    ));
    let open = render(&app, 120, 40);
    assert!(
        open.contains("User bob is browsing your list of shared files"),
        "{open}"
    );
    assert!(
        open.contains("User bob is searching for \"notes\", found 1 results"),
        "{open}"
    );
    assert!(open.contains("log"), "{open}");
    press(&mut app, KeyCode::Char('`'));
    assert!(!app.log_open());
    let closed = render(&app, 120, 40);
    assert!(closed.contains('▸'), "{closed}");
    assert!(
        closed.contains("User bob is searching for \"notes\", found 1 results"),
        "{closed}"
    );
    assert!(
        !closed.contains("User bob is browsing your list of shared files"),
        "{closed}"
    );
    press(&mut app, KeyCode::Char('`'));
    let open = render(&app, 120, 40);
    assert!(
        open.contains("User bob is browsing your list of shared files"),
        "{open}"
    );
}

#[test]
fn a_chat_message_is_visible_while_typing() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    let idle = render(&app, 120, 40);
    assert!(idle.contains("type a message"), "{idle}");
    for ch in ['h', 'e', 'l', 'l', 'o'] {
        press(&mut app, KeyCode::Char(ch));
    }
    let typing = render(&app, 120, 40);
    assert!(typing.contains("hello█"), "{typing}");
    assert_eq!(app.view(), View::Chat);
}

#[test]
fn the_room_list_puts_the_largest_room_first() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    app.apply_session(SessionEvent::RoomListed {
        room: "room-zebra".to_owned(),
        users: 200,
    });
    app.apply_session(SessionEvent::RoomListed {
        room: "room-alpha".to_owned(),
        users: 200,
    });
    let text = render(&app, 120, 40);
    let lines: Vec<&str> = text.lines().collect();
    let alpha = lines
        .iter()
        .position(|line| line.contains("room-alpha  200"))
        .unwrap_or_else(|| panic!("{text}"));
    let zebra = lines
        .iter()
        .position(|line| line.contains("room-zebra  200"))
        .unwrap_or_else(|| panic!("{text}"));
    let music = lines
        .iter()
        .position(|line| line.contains("preview-music  128"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(alpha < zebra, "{text}");
    assert!(zebra < music, "{text}");
}

#[test]
fn a_long_chat_line_wraps_when_the_transcript_narrows() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    let body = "violet lanterns over the ridge";
    app.apply_session(SessionEvent::Chat(ChatLine {
        room: "preview-music".to_owned(),
        time: "18:04".to_owned(),
        user: "ada".to_owned(),
        text: body.to_owned(),
    }));
    let wide = render(&app, 120, 40);
    assert!(wide.contains(body), "{wide}");
    assert!(
        wide.lines()
            .any(|line| line.contains("violet") && line.contains("ridge")),
        "{wide}"
    );
    let narrow = render(&app, 70, 40);
    assert!(narrow.contains("violet"), "{narrow}");
    assert!(narrow.contains("ridge"), "{narrow}");
    assert!(
        narrow
            .lines()
            .any(|line| line.contains("violet") && !line.contains("ridge")),
        "{narrow}"
    );
    assert!(
        narrow
            .lines()
            .any(|line| line.contains("ridge") && !line.contains("violet")),
        "{narrow}"
    );
}

#[test]
fn a_long_draft_wraps_onto_a_second_row() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    for ch in "abcdefghijklmnopqrstuvwxyz".chars() {
        press(&mut app, KeyCode::Char(ch));
    }
    let text = render(&app, 70, 40);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains("abcdefghij"))
        .unwrap_or_else(|| panic!("{text}"));
    let cursor = lines
        .iter()
        .position(|line| line.contains('█'))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(start < cursor, "{text}");
    assert!(!lines[start].contains('█'), "{text}");
}

#[test]
fn the_member_list_is_alphabetical() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    app.apply_session(SessionEvent::RoomMembers {
        room: "preview-music".to_owned(),
        members: vec![
            RoomPerson::named("zoe"),
            RoomPerson::named("amy"),
            RoomPerson::named("mia"),
        ],
    });
    let text = render(&app, 120, 40);
    let lines: Vec<&str> = text.lines().collect();
    let amy = lines
        .iter()
        .position(|line| line.contains("amy"))
        .unwrap_or_else(|| panic!("{text}"));
    let mia = lines
        .iter()
        .position(|line| line.contains("mia"))
        .unwrap_or_else(|| panic!("{text}"));
    let zoe = lines
        .iter()
        .position(|line| line.contains("zoe"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(amy < mia, "{text}");
    assert!(mia < zoe, "{text}");
}

#[test]
fn a_room_member_card_shows_browse_and_friend() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    app.apply_session(SessionEvent::RoomMembers {
        room: "preview-music".to_owned(),
        members: vec![RoomPerson {
            name: "bob".to_owned(),
            status: Some(2),
            country: "US".to_owned(),
            files: Some(12),
            dirs: Some(3),
        }],
    });
    app.on_pointer(Target::Person(0));
    let text = render(&app, 120, 40);
    assert!(text.contains("bob"), "{text}");
    assert!(text.contains("online"), "{text}");
    assert!(text.contains("browse files"), "{text}");
    assert!(text.contains("add friend"), "{text}");
    assert!(text.contains("ignore"), "{text}");
}

#[test]
fn sign_in_is_the_opening_screen() {
    let text = render(&App::sign_in(), 120, 40);
    assert!(text.contains("soul sever"), "{text}");
    assert!(text.contains("sign in"), "{text}");
    assert!(
        text.contains("A name that is not already registered")
            && text.contains("creates a new Soulseek account."),
        "{text}"
    );
    assert!(text.contains("username"), "{text}");
    assert!(text.contains("password"), "{text}");
    assert!(!text.contains("OFFLINE PREVIEW"), "{text}");
    assert!(!text.contains("preview-"), "{text}");
    assert!(text.contains("╭"), "{text}");
    let narrow = render(&App::sign_in(), 70, 18);
    assert!(
        narrow.lines().any(|line| {
            line.contains("A name that is not already registered") && !line.contains("creates")
        }),
        "{narrow}"
    );
    assert!(
        narrow
            .lines()
            .any(|line| line.contains("creates a new Soulseek account.")),
        "{narrow}"
    );
}

#[test]
fn upload_slots_follow_the_configured_count() {
    let mut app = App::preview();
    let mut config = soul_sever::config::Config::default();
    config.upload_slots = 5;
    app.bind_config(std::path::PathBuf::from("unused.toml"), config);
    app.set_account("alice");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(127, 0, 0, 1),
        supporter: false,
    });
    app.set_view(View::Uploads);
    let empty = render(&app, 120, 40);
    assert!(empty.contains("slots 0/5"), "{empty}");
    app.apply_session(SessionEvent::Transfer(Transfer {
        direction: Direction::Upload,
        user: "bob".to_owned(),
        path: "a.flac".to_owned(),
        size: 10,
        done: 1,
        speed: 1,
        queue: None,
        state: TransferState::Transferring,
        detail: String::new(),
    }));
    let busy = render(&app, 120, 40);
    assert!(busy.contains("slots 1/5"), "{busy}");
    app.set_view(View::Dashboard);
    let dash = render(&app, 120, 40);
    assert!(dash.contains("1/5"), "{dash}");
}

#[test]
fn each_view_renders_its_title() {
    let mut app = App::preview();
    let expected = [
        (View::Search, "query"),
        (View::Downloads, "download queue"),
        (View::Uploads, "upload queue"),
        (View::Browse, "browse"),
        (View::Chat, "ticker · —"),
        (View::Users, "buddies · ignored · banned"),
        (View::Shares, "public shares"),
        (View::Settings, "enter edits"),
    ];
    for (view, marker) in expected {
        app.set_view(view);
        let text = render(&app, 120, 40);
        assert!(
            text.contains(marker),
            "missing {marker} in {view:?}\n{text}"
        );
    }
}

#[test]
fn downloads_and_uploads_say_x_removes_the_row() {
    let mut app = App::preview();
    app.set_view(View::Downloads);
    let downloads = render(&app, 120, 40);
    assert!(downloads.contains("x removes the row"), "{downloads}");
    app.set_view(View::Uploads);
    let uploads = render(&app, 120, 40);
    assert!(uploads.contains("X clears finished"), "{uploads}");
}

#[test]
fn a_saved_account_drops_signing_in_after_login() {
    let mut app = App::connecting("ada");
    let before = render(&app, 120, 40);
    assert!(before.contains("signing in"), "{before}");

    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(127, 0, 0, 1),
        supporter: false,
    });
    let text = render(&app, 120, 40);
    assert!(text.contains("ada"), "{text}");
    assert!(text.contains("hello"), "{text}");
    assert!(!text.contains("signing in"), "{text}");
}

#[test]
fn the_header_shows_the_interface_address() {
    let mut app = App::blank();
    app.set_account("alice");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(203, 0, 113, 8),
        supporter: false,
    });
    app.apply_session(SessionEvent::Listening {
        port: 2234,
        address: "10.8.0.5".to_owned(),
    });
    let text = render(&app, 120, 40);
    assert!(text.contains("10.8.0.5"), "{text}");
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("hello"), "{text}");
}

#[test]
fn logged_in_header_replaces_the_preview_banner() {
    let mut app = App::blank();
    app.set_account("alice");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(127, 0, 0, 1),
        supporter: true,
    });
    let text = render(&app, 120, 40);
    assert!(text.contains("alice"), "{text}");
    assert!(text.contains("hello"), "{text}");
    assert!(!text.contains("OFFLINE PREVIEW"), "{text}");
    assert!(!text.contains("preview-"), "{text}");
}

#[test]
fn login_failure_shows_the_reason() {
    let mut app = App::connecting("ada");
    app.apply_session(SessionEvent::LoginFailed {
        reason: "INVALIDPASS".to_owned(),
    });
    let text = render(&app, 120, 40);
    assert!(text.contains("INVALIDPASS"), "{text}");
    assert!(text.contains("username"), "{text}");
    assert!(text.contains("ada"), "{text}");
    assert_eq!(app.notice(), "INVALIDPASS");
    assert!(!text.contains("OFFLINE PREVIEW"), "{text}");
    assert!(!text.contains("preview-"), "{text}");
}

#[test]
fn help_and_resize_are_visible() {
    let mut app = App::preview();
    press(&mut app, KeyCode::Char('?'));
    let text = render(&app, 120, 40);
    assert!(text.contains("switch view"), "{text}");
    assert!(text.contains("download the highlighted"), "{text}");
    assert!(text.contains('█'), "{text}");
    assert!(
        text.contains("-by E.N. Ramos <enramos@live.com>-"),
        "{text}"
    );
    let art = text.find('█').unwrap();
    let credit = text.find("-by E.N. Ramos <enramos@live.com>-").unwrap();
    let sheet = text.find("switch view").unwrap();
    assert!(art < credit && credit < sheet, "{text}");

    let small = render(&App::preview(), 40, 10);
    assert!(small.contains("terminal too small"), "{small}");
}

#[test]
fn sign_in_asks_for_a_username_and_hides_the_password() {
    let mut app = App::sign_in();
    for ch in ['a', 'd', 'a'] {
        press(&mut app, KeyCode::Char(ch));
    }
    press(&mut app, KeyCode::Enter);
    for ch in ['s', 'e', 'c', 'r', 'e', 't'] {
        press(&mut app, KeyCode::Char(ch));
    }
    let text = render(&app, 120, 40);
    assert!(text.contains("sign in"), "{text}");
    assert!(text.contains("username"), "{text}");
    assert!(text.contains("password"), "{text}");
    assert!(text.contains("ada"), "{text}");
    assert!(text.contains("******"), "{text}");
    assert!(!text.contains("secret"), "{text}");
    assert!(!text.contains("OFFLINE PREVIEW"), "{text}");
    assert!(!text.contains("preview-alice"), "{text}");
}

#[test]
fn live_feed_with_one_transfer_hides_preview_alice() {
    let mut app = App::blank();
    app.set_account("alice");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(127, 0, 0, 1),
        supporter: true,
    });
    app.apply_session(SessionEvent::Transfer(soul_sever::model::Transfer {
        direction: soul_sever::model::Direction::Download,
        user: "bob".to_owned(),
        path: "tone.bin".to_owned(),
        size: 10_000,
        done: 5_000,
        speed: 0,
        queue: None,
        state: soul_sever::model::TransferState::Transferring,
        detail: String::new(),
    }));
    app.apply_session(SessionEvent::Shares {
        public_files: 1,
        public_folders: 1,
        buddy_files: 0,
        buddy_folders: 0,
        trusted_files: 0,
        trusted_folders: 0,
    });
    for view in View::ALL {
        app.set_view(view);
        let text = render(&app, 120, 40);
        assert!(
            !text.contains("preview-"),
            "{view:?} still shows a preview row\n{text}"
        );
    }
    app.set_view(View::Dashboard);
    let text = render(&app, 120, 40);
    assert!(text.contains("bob"), "{text}");
    assert!(text.contains("tone.bin"), "{text}");
    assert!(text.contains('⣿'), "{text}");
    assert!(text.contains('⣀'), "{text}");
    assert!(text.contains("50%"), "{text}");
    app.set_view(View::Shares);
    let text = render(&app, 120, 40);
    assert!(text.contains("1 files"), "{text}");
    assert!(text.contains("1 folders"), "{text}");
}

#[test]
fn mouse_hits_follow_tabs_rows_query_and_help() {
    let app = App::preview();
    let (_text, pointer) = render_pointer(&app, 120, 40);

    let tab = pointer
        .region(|target| target == Target::OpenView(View::Downloads))
        .expect("downloads tab");
    let mut app = App::preview();
    app.on_pointer(pointer.at(tab.x, tab.y).unwrap());
    assert_eq!(app.view(), View::Downloads);

    let (_text, pointer) = render_pointer(&app, 120, 40);
    let row = pointer
        .region(|target| target == Target::Select(1))
        .expect("second download row");
    app.on_pointer(pointer.at(row.x + 1, row.y).unwrap());
    assert_eq!(app.cursor(), 1);
    assert!(matches!(
        pointer.at(row.x + 1, row.y),
        Some(Target::Select(1))
    ));
    app.on_scroll(false);
    assert_eq!(app.cursor(), 1);

    app.set_view(View::Search);
    let (_text, pointer) = render_pointer(&app, 120, 40);
    let query = pointer
        .region(|target| target == Target::FocusQuery)
        .expect("query line");
    app.on_pointer(pointer.at(query.x + 2, query.y).unwrap());
    assert!(app.editing());
    let mode = pointer
        .region(|target| target == Target::CycleMode)
        .expect("mode line");
    app.on_pointer(pointer.at(mode.x + 2, mode.y).unwrap());
    assert_eq!(app.mode(), SearchMode::Room);

    let help = pointer
        .region(|target| target == Target::OpenHelp)
        .expect("help button");
    app.on_pointer(pointer.at(help.x, help.y).unwrap());
    assert!(app.help());
}

#[test]
fn search_results_say_d_downloads_the_album() {
    let mut app = App::preview();
    app.set_account("ada");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::new(127, 0, 0, 1),
        supporter: true,
    });
    app.set_view(View::Search);
    for path in ["music\\Dookie\\01.flac", "music\\Dookie\\02.flac"] {
        app.apply_session(SessionEvent::SearchResult(
            0,
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
            },
        ));
    }
    let text = render(&app, 120, 40);
    assert!(text.contains("d downloads this album"), "{text}");
    assert!(text.contains("2 results"), "{text}");
}

#[test]
fn shares_view_asks_for_a_folder_path() {
    let mut app = App::preview();
    app.set_view(View::Shares);
    let text = render(&app, 120, 40);
    assert!(text.contains("public shares"), "{text}");
    assert!(text.contains("enter picks a folder"), "{text}");
    assert!(text.contains("no folders yet"), "{text}");

    app.open_folder_picker(std::env::temp_dir());
    let text = render(&app, 120, 40);
    assert!(text.contains("share this folder"), "{text}");
    assert!(text.contains("s shares this folder"), "{text}");
}

#[test]
fn quit_key_ends_the_session() {
    let mut app = App::preview();
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit());
}

#[test]
fn every_split_exposes_both_edges() {
    let mut app = App::preview();
    let checks = [
        (View::Chat, Split::ChatRooms, Edge::East),
        (View::Chat, Split::ChatRooms, Edge::West),
        (View::Chat, Split::ChatMembers, Edge::East),
        (View::Chat, Split::ChatMembers, Edge::West),
        (View::Search, Split::SearchQuery, Edge::South),
        (View::Search, Split::SearchQuery, Edge::North),
        (View::Search, Split::SearchFilters, Edge::East),
        (View::Search, Split::SearchFilters, Edge::West),
        (View::Users, Split::UsersDetail, Edge::West),
        (View::Users, Split::UsersDetail, Edge::East),
        (View::Settings, Split::SettingsNav, Edge::East),
        (View::Settings, Split::SettingsNav, Edge::West),
        (View::Shares, Split::SharesLeft, Edge::East),
        (View::Shares, Split::SharesLeft, Edge::West),
        (View::Shares, Split::SharesRight, Edge::East),
        (View::Shares, Split::SharesRight, Edge::West),
        (View::Dashboard, Split::DashMeters, Edge::South),
        (View::Dashboard, Split::DashMeters, Edge::North),
        (View::Dashboard, Split::DashNetwork, Edge::West),
        (View::Dashboard, Split::DashNetwork, Edge::East),
        (View::Dashboard, Split::DashDownload, Edge::East),
        (View::Dashboard, Split::DashDownload, Edge::West),
    ];
    for (view, split, edge) in checks {
        app.set_view(view);
        let (_, pointer) = render_pointer(&app, 120, 40);
        assert!(
            pointer
                .region(|target| target == Target::Resize(split, edge))
                .is_some(),
            "missing {split:?} {edge:?} on {view:?}"
        );
    }
}

#[test]
fn dragging_chat_east_stops_at_the_rooms_default_and_west_widens_the_text() {
    let mut app = App::preview();
    app.set_view(View::Chat);
    let (_, pointer) = render_pointer(&app, 120, 40);
    let east = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::East))
        .unwrap();
    let (rooms, text, members) = app.chat_widths(120);
    assert_eq!((rooms, text, members), (24, 76, 20));

    app.begin_resize(Split::ChatRooms, Edge::East, east.x, east.y);
    assert!(app.resize_to(east.x + 8, east.y));
    assert_eq!(app.chat_widths(120), (32, 68, 20));
    let (_, pointer) = render_pointer(&app, 120, 40);
    let moved = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::East))
        .unwrap();
    assert_eq!(moved.x, east.x + 8);

    assert!(app.resize_to(east.x.saturating_sub(40), east.y));
    assert_eq!(app.chat_widths(120), (24, 76, 20));
    let (_, pointer) = render_pointer(&app, 120, 40);
    let restored = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::East))
        .unwrap();
    assert_eq!(restored.x, east.x);
    app.end_resize();

    let west = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::West))
        .unwrap();
    app.begin_resize(Split::ChatRooms, Edge::West, west.x, west.y);
    assert!(app.resize_to(west.x.saturating_sub(10), west.y));
    assert_eq!(app.chat_widths(120), (14, 86, 20));
    let (_, pointer) = render_pointer(&app, 120, 40);
    let widened = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::West))
        .unwrap();
    assert_eq!(widened.x, west.x - 10);

    assert!(app.resize_to(west.x + 40, west.y));
    assert_eq!(app.chat_widths(120), (24, 76, 20));
    let (_, pointer) = render_pointer(&app, 120, 40);
    let back = pointer
        .region(|target| target == Target::Resize(Split::ChatRooms, Edge::West))
        .unwrap();
    assert_eq!(back.x, west.x);
}

#[test]
fn dragging_the_search_query_south_stops_at_the_original_height() {
    let mut app = App::preview();
    app.set_view(View::Search);
    let (_, pointer) = render_pointer(&app, 120, 40);
    let south = pointer
        .region(|target| target == Target::Resize(Split::SearchQuery, Edge::South))
        .unwrap();
    app.begin_resize(Split::SearchQuery, Edge::South, south.x, south.y);
    assert!(app.resize_to(south.x, south.y + 3));
    assert_eq!(app.search_query_height(37), 8);
    let (_, pointer) = render_pointer(&app, 120, 40);
    let moved = pointer
        .region(|target| target == Target::Resize(Split::SearchQuery, Edge::South))
        .unwrap();
    assert_eq!(moved.y, south.y + 3);

    assert!(app.resize_to(south.x, south.y.saturating_sub(20)));
    assert_eq!(app.search_query_height(37), 5);
    let (_, pointer) = render_pointer(&app, 120, 40);
    let restored = pointer
        .region(|target| target == Target::Resize(Split::SearchQuery, Edge::South))
        .unwrap();
    assert_eq!(restored.y, south.y);
    app.end_resize();

    let north = pointer
        .region(|target| target == Target::Resize(Split::SearchQuery, Edge::North))
        .unwrap();
    app.begin_resize(Split::SearchQuery, Edge::North, north.x, north.y);
    assert!(app.resize_to(north.x, north.y.saturating_sub(2)));
    assert_eq!(app.search_query_height(37), 3);
    assert!(app.resize_to(north.x, north.y + 30));
    assert_eq!(app.search_query_height(37), 5);
}

#[test]
fn the_library_nests_albums_under_the_artist_and_draws_the_spectrum() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-library-screen-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    let nice = root.join("nice.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    write_tagged(&nice, "Green Day", "Nimrod", "Nice Guys Finish Last", 1);
    let filed_basket = soul_sever::library::shelf(&root, &basket).unwrap();
    let filed_nice = soul_sever::library::shelf(&root, &nice).unwrap();
    std::fs::create_dir_all(filed_basket.parent().unwrap()).unwrap();
    std::fs::create_dir_all(filed_nice.parent().unwrap()).unwrap();
    std::fs::rename(&basket, &filed_basket).unwrap();
    std::fs::rename(&nice, &filed_nice).unwrap();

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    settle_library(&mut app);
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
    let (text, pointer) = render_pointer(&app, 120, 40);
    assert!(text.contains("play"), "{text}");
    assert!(text.contains("Green Day"), "{text}");
    assert!(text.contains("Dookie"), "{text}");
    assert!(text.contains("Nimrod"), "{text}");
    assert!(text.contains("Basket Case"), "{text}");
    assert!(text.contains("nothing playing"), "{text}");
    assert!(text.contains('●'), "{text}");
    assert!(!text.contains("▸ log"), "{text}");
    assert!(!text.contains("log  ·"), "{text}");
    assert!(
        pointer
            .region(|target| target == Target::Column(2, 0))
            .is_some()
    );
    press(&mut app, KeyCode::Enter);
    wait_until_ready(&app);
    assert!(app.track_rows().iter().any(|row| row == "Basket Case"));
    app.on_pointer(Target::Column(1, 1));
    assert!(app.track_rows().iter().any(|row| row == "Basket Case"));
    let (_, albums) = render_pointer(&app, 120, 40);
    assert!(
        albums
            .region(|target| target == Target::Column(2, 0))
            .is_some()
    );
    app.on_pointer(Target::Column(2, 0));
    wait_until_ready(&app);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
    let (playing, transport) = render_pointer(&app, 120, 40);
    assert!(playing.contains("Nice Guys Finish Last"), "{playing}");
    assert!(playing.contains("playing"), "{playing}");
    assert!(playing.contains("WAV"), "{playing}");
    assert!(playing.contains("kHz"), "{playing}");
    assert!(playing.contains("KB"), "{playing}");
    assert!(playing.contains('▶'), "{playing}");
    assert!(playing.contains("❚❚"), "{playing}");
    assert!(playing.contains('■'), "{playing}");
    assert!(!playing.contains("██████████"), "{playing}");
    assert!(playing.contains('⣿'), "{playing}");
    assert!(!playing.contains('▀'), "{playing}");
    assert!(!playing.contains("▸ log"), "{playing}");
    for target in [Target::Play, Target::Pause, Target::Stop] {
        let hit = transport
            .region(|item| item == target)
            .unwrap_or_else(|| panic!("missing {target:?}"));
        assert_eq!(hit.height, 1, "{target:?}");
        assert!(
            hit.width >= 1 && hit.width <= 3,
            "{target:?} width {}",
            hit.width
        );
    }
    let bar = transport.region(|target| target == Target::Seek).unwrap();
    let (start, _, _) = app.transport();
    app.seek_column(bar.x + bar.width / 2, bar.x, bar.width);
    app.on_pointer(Target::Pause);
    assert!(
        app.track_rows().iter().any(|row| row == "paused"),
        "{:?}",
        app.track_rows()
    );
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
    app.on_pointer(Target::Play);
    assert!(
        app.track_rows().iter().any(|row| row.contains("playing")),
        "{:?}",
        app.track_rows()
    );
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
    app.on_pointer(Target::Stop);
    assert_eq!(app.transport().0, "0:00.00");
    assert!(
        app.track_rows().iter().any(|row| row == "stopped"),
        "{:?}",
        app.track_rows()
    );
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
    app.seek_column(bar.x + bar.width.saturating_sub(1), bar.x, bar.width);
    let (later, _, _) = app.transport();
    assert_ne!(start, later, "{start} -> {later}");
    assert_ne!(later, "0:00.00");
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.transport().0, "0:00.00");
    assert!(
        app.track_rows().iter().any(|row| row == "stopped"),
        "{:?}",
        app.track_rows()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_a_library_item_asks_before_removing_the_file() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-delete-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    let nice = root.join("nice.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    write_tagged(&nice, "Green Day", "Nimrod", "Nice Guys Finish Last", 1);
    let filed_basket = soul_sever::library::shelf(&root, &basket).unwrap();
    let filed_nice = soul_sever::library::shelf(&root, &nice).unwrap();
    std::fs::create_dir_all(filed_basket.parent().unwrap()).unwrap();
    std::fs::create_dir_all(filed_nice.parent().unwrap()).unwrap();
    std::fs::rename(&basket, &filed_basket).unwrap();
    std::fs::rename(&nice, &filed_nice).unwrap();

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    settle_library(&mut app);
    press(&mut app, KeyCode::Char('0'));

    press(&mut app, KeyCode::Char('x'));
    let asked = render(&app, 120, 40);
    assert!(asked.contains("delete"), "{asked}");
    assert!(asked.contains("Remove Green Day from disk?"), "{asked}");
    assert!(asked.contains("2 files will be deleted."), "{asked}");
    assert!(asked.contains("enter removes it"), "{asked}");
    assert!(filed_basket.is_file());
    assert!(filed_nice.is_file());
    press(&mut app, KeyCode::Esc);
    assert!(app.delete_prompt().is_none());
    assert!(filed_basket.is_file());

    press(&mut app, KeyCode::Char(']'));
    press(&mut app, KeyCode::Char(']'));
    press(&mut app, KeyCode::Enter);
    wait_until_ready(&app);
    press(&mut app, KeyCode::Char('x'));
    let (song_prompt, pointer) = render_pointer(&app, 120, 40);
    assert!(
        song_prompt.contains("Remove Basket Case from disk?"),
        "{song_prompt}"
    );
    assert!(
        song_prompt.contains("This file will be deleted."),
        "{song_prompt}"
    );
    let cancel = pointer
        .region(|target| target == Target::Confirm(false))
        .expect("cancel");
    app.on_pointer(pointer.at(cancel.x, cancel.y).unwrap());
    assert!(app.delete_prompt().is_none());
    assert!(filed_basket.is_file());

    press(&mut app, KeyCode::Char('x'));
    let remove = render_pointer(&app, 120, 40)
        .1
        .region(|target| target == Target::Confirm(true))
        .expect("remove");
    assert!(remove.width >= 1);
    press(&mut app, KeyCode::Enter);
    assert!(!filed_basket.exists());
    assert!(filed_nice.is_file());
    assert_eq!(app.take_forget(), vec![filed_basket.clone()]);
    assert!(
        app.track_rows().iter().any(|row| row == "nothing playing"),
        "{:?}",
        app.track_rows()
    );
    let left = render(&app, 120, 40);
    assert!(left.contains("Nice Guys Finish Last"), "{left}");
    assert!(!left.contains("Basket Case"), "{left}");

    press(&mut app, KeyCode::Char('['));
    press(&mut app, KeyCode::Char('x'));
    let album = render(&app, 120, 40);
    assert!(album.contains("Remove Nimrod from disk?"), "{album}");
    assert!(album.contains("1 file will be deleted."), "{album}");
    press(&mut app, KeyCode::Enter);
    assert!(!filed_nice.exists());
    assert_eq!(app.take_forget(), vec![filed_nice]);
    assert!(app.artist_names().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn opening_the_library_switches_before_the_scan_finishes() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-open-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    let filed = soul_sever::library::shelf(&root, &basket).unwrap();
    std::fs::create_dir_all(filed.parent().unwrap()).unwrap();
    std::fs::rename(&basket, &filed).unwrap();

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    settle_library(&mut app);
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(app.view(), View::Library);
    assert!(app.library_scan_pending());
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.library_scan_pending());
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_finished_download_while_playing_leaves_the_meter_free() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-meter-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    let filed = soul_sever::library::shelf(&root, &basket).unwrap();
    std::fs::create_dir_all(filed.parent().unwrap()).unwrap();
    std::fs::rename(&basket, &filed).unwrap();

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    settle_library(&mut app);
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    press(&mut app, KeyCode::Enter);
    wait_until_ready(&app);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let extra = root
        .join("Ella Langley")
        .join("Hungover")
        .join("01 You.wav");
    std::fs::create_dir_all(extra.parent().unwrap()).unwrap();
    write_tagged(
        &extra,
        "Ella Langley",
        "Hungover",
        "You Look Like You Love Me",
        1,
    );
    app.apply_session(SessionEvent::Transfer(Transfer {
        direction: Direction::Download,
        user: "bob".to_owned(),
        path: "music\\Hungover\\01.wav".to_owned(),
        size: 10,
        done: 10,
        speed: 0,
        queue: None,
        state: TransferState::Finished,
        detail: String::new(),
    }));
    assert!(app.library_scan_pending());
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    assert_eq!(app.spectrum().len(), 96);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(33));
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.library_scan_pending());
    assert!(app.artist_names().iter().any(|name| name == "Ella Langley"));
    assert!(app.artist_names().iter().any(|name| name == "Green Day"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_finished_download_leaves_the_download_view_free() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-move-ui-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    let filed = soul_sever::library::shelf(&root, &basket).unwrap();
    std::fs::create_dir_all(filed.parent().unwrap()).unwrap();
    std::fs::rename(&basket, &filed).unwrap();

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    settle_library(&mut app);
    press(&mut app, KeyCode::Char('0'));
    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let extra = root
        .join("Ella Langley")
        .join("Hungover")
        .join("01 You.wav");
    std::fs::create_dir_all(extra.parent().unwrap()).unwrap();
    write_tagged(
        &extra,
        "Ella Langley",
        "Hungover",
        "You Look Like You Love Me",
        1,
    );
    app.apply_session(SessionEvent::Transfer(Transfer {
        direction: Direction::Download,
        user: "bob".to_owned(),
        path: "music\\Hungover\\01.wav".to_owned(),
        size: 10,
        done: 10,
        speed: 0,
        queue: None,
        state: TransferState::Finished,
        detail: String::new(),
    }));
    assert!(app.library_scan_pending());
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    assert_eq!(app.frame_wait(), std::time::Duration::from_millis(200));
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.library_scan_pending());
    assert!(app.artist_names().iter().any(|name| name == "Ella Langley"));
    assert!(app.artist_names().iter().any(|name| name == "Green Day"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_guest_folder_shows_in_the_library_and_is_removed() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-guest-ui-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let guest = root
        .join("Eminem f Thyme")
        .join("Infinite")
        .join("07 Open Mic.wav");
    std::fs::create_dir_all(guest.parent().unwrap()).unwrap();
    write_tagged(&guest, "Eminem f Thyme", "Infinite", "Open Mic", 7);

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(root.join("config.toml"), config);
    let started = std::time::Instant::now();
    while app.artist_names().is_empty() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        app.artist_names().iter().any(|name| name == "Eminem"),
        "{:?}",
        app.artist_names()
    );
    assert!(
        !app.artist_names().iter().any(|name| name.contains("Thyme")),
        "{:?}",
        app.artist_names()
    );
    settle_library(&mut app);
    assert!(!root.join("Eminem f Thyme").exists());
    assert!(
        root.join("Eminem")
            .join("Infinite")
            .join("07 Open Mic.wav")
            .is_file()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_saved_library_is_on_screen_before_the_folder_is_reread() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-saved-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    let catalog = soul_sever::library::Catalog::scan(&root);
    let config_dir = root.join("account");
    std::fs::create_dir_all(&config_dir).unwrap();
    catalog.store(&config_dir.join("library.toml"), &root);

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(config_dir.join("config.toml"), config);
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    settle_library(&mut app);
    assert_eq!(app.artist_names(), vec!["Green Day".to_owned()]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_quality_view_names_measuring_and_replacing() {
    let mut app = App::preview();
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.view(), View::Quality);
    let text = render(&app, 120, 40);
    assert!(text.contains("spec"), "{text}");
    assert!(text.contains("measuring"), "{text}");
    assert!(text.contains("replacing"), "{text}");
    assert!(!text.contains("searching"), "{text}");
    assert!(text.contains("idle"), "{text}");
    assert!(text.contains("[ ] column"), "{text}");
    assert!(text.contains("r checks again"), "{text}");
    press(&mut app, KeyCode::Char('?'));
    let help = render(&app, 120, 40);
    assert!(help.contains("check quality again"), "{help}");
}

#[test]
fn the_track_panel_shows_a_saved_lossy_percent() {
    let root = std::env::temp_dir().join(format!(
        "soul-sever-lossy-row-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let basket = root.join("basket.wav");
    write_tagged(&basket, "Green Day", "Dookie", "Basket Case", 1);
    let mut catalog = soul_sever::library::Catalog::scan(&root);
    let path = catalog.artists[0].albums[0].songs[0].path.clone();
    catalog.set_quality(
        &path,
        soul_sever::quality::Quality {
            bit_depth: 16,
            sample_rate: 44_100,
            lossy_percent: Some(27),
            cutoff_hz: 16_000,
            duration_secs: Some(1),
            verdict: soul_sever::quality::Verdict::Lossy,
            upgrade_attempted: false,
        },
    );
    let config_dir = root.join("account");
    std::fs::create_dir_all(&config_dir).unwrap();
    catalog.store(&config_dir.join("library.toml"), &root);

    let mut config = soul_sever::config::Config::default();
    config.download_dir = root.to_string_lossy().into_owned();
    let mut app = App::preview();
    app.bind_config(config_dir.join("config.toml"), config);
    press(&mut app, KeyCode::Char('0'));
    assert!(
        app.track_rows().iter().any(|row| row == "nothing playing"),
        "{:?}",
        app.track_rows()
    );
    assert!(
        app.track_rows().iter().any(|row| row == "lossy · 27%"),
        "{:?}",
        app.track_rows()
    );
    let text = render(&app, 120, 40);
    assert!(text.contains("nothing playing"), "{text}");
    assert!(text.contains("lossy · 27%"), "{text}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_short_queue_has_no_thumb() {
    let app = App::preview();
    let (text, pointer) = render_pointer(&app, 120, 40);
    assert!(!text.contains('█'), "{text}");
    assert!(pointer.metrics(ScrollId::List).is_none());
}

#[test]
fn a_long_room_list_shows_a_thumb_that_can_be_dragged() {
    let mut app = App::blank();
    app.set_view(View::Chat);
    for index in 0..40 {
        app.apply_session(SessionEvent::RoomListed {
            room: format!("room-{index:02}"),
            users: 1_000 - index,
        });
    }
    let (text, pointer) = render_pointer(&app, 100, 30);
    assert!(text.contains('█'), "{text}");
    assert!(text.contains("room-00"), "{text}");
    let track = pointer
        .region(|target| target == Target::VScroll(ScrollId::List))
        .expect("room scrollbar");
    let metrics = pointer.metrics(ScrollId::List).expect("room metrics");
    app.begin_scroll(ScrollId::List, track.y, metrics);
    app.drag_scroll(track.y.saturating_add(track.height.saturating_sub(1)));
    app.end_scroll();
    let text = render(&app, 100, 30);
    assert_eq!(app.cursor(), 0);
    assert_eq!(app.rooms()[app.cursor()].name, "room-00");
    assert!(!text.contains("   room-00"), "{text}");
    assert!(text.contains("room-30"), "{text}");
}

#[test]
fn a_long_member_list_and_download_queue_show_a_thumb() {
    let mut app = App::blank();
    app.set_view(View::Chat);
    app.apply_session(SessionEvent::RoomListed {
        room: "hall".to_owned(),
        users: 40,
    });
    app.apply_session(SessionEvent::RoomMembers {
        room: "hall".to_owned(),
        members: (0..40)
            .map(|index| soul_sever::model::RoomPerson::named(format!("person-{index:02}")))
            .collect(),
    });
    let (text, pointer) = render_pointer(&app, 120, 30);
    assert!(pointer.metrics(ScrollId::Members).is_some(), "{text}");
    assert!(text.contains('█'), "{text}");

    let mut app = App::blank();
    app.set_account("alice");
    app.apply_session(SessionEvent::LoggedIn {
        banner: "hello".to_owned(),
        ip: Ipv4Addr::LOCALHOST,
        supporter: false,
    });
    app.set_view(View::Downloads);
    for index in 0..40 {
        app.apply_session(SessionEvent::Transfer(soul_sever::model::Transfer {
            direction: Direction::Download,
            user: "bob".to_owned(),
            path: format!("queue-{index:02}.bin"),
            size: 10,
            done: 0,
            speed: 0,
            queue: None,
            state: TransferState::Queued,
            detail: String::new(),
        }));
    }
    let (text, pointer) = render_pointer(&app, 120, 30);
    assert!(pointer.metrics(ScrollId::List).is_some(), "{text}");
    assert!(text.contains("queue-00"), "{text}");
    assert!(text.contains('█'), "{text}");
}

#[test]
fn the_transcript_pins_to_the_newest_line_until_the_thumb_moves() {
    let mut app = App::blank();
    app.set_view(View::Chat);
    app.apply_session(SessionEvent::RoomListed {
        room: "hall".to_owned(),
        users: 2,
    });
    for index in 0..40 {
        app.apply_session(SessionEvent::Chat(ChatLine {
            room: "hall".to_owned(),
            time: "00:00".to_owned(),
            user: "ada".to_owned(),
            text: format!("line-{index:02}"),
        }));
    }
    let (text, pointer) = render_pointer(&app, 120, 30);
    assert!(text.contains("line-39"), "{text}");
    assert!(!text.contains("line-00"), "{text}");
    let say = pointer
        .region(|target| target == Target::FocusQuery)
        .expect("composer");
    assert!(!matches!(
        pointer.at(say.x + 1, say.y),
        Some(Target::VScroll(_) | Target::Wheel(_))
    ));
    let track = pointer
        .region(|target| target == Target::VScroll(ScrollId::Transcript))
        .expect("transcript scrollbar");
    let metrics = pointer
        .metrics(ScrollId::Transcript)
        .expect("transcript metrics");
    app.begin_scroll(
        ScrollId::Transcript,
        track.y.saturating_add(track.height.saturating_sub(1)),
        metrics,
    );
    app.drag_scroll(track.y);
    app.end_scroll();
    let text = render(&app, 120, 30);
    assert!(text.contains("line-00"), "{text}");
    assert!(!text.contains("line-39"), "{text}");
    app.apply_session(SessionEvent::Chat(ChatLine {
        room: "hall".to_owned(),
        time: "00:01".to_owned(),
        user: "ada".to_owned(),
        text: "line-newest".to_owned(),
    }));
    let text = render(&app, 120, 30);
    assert!(text.contains("line-00"), "{text}");
    assert!(!text.contains("line-newest"), "{text}");
}

#[test]
fn share_paths_and_setting_fields_keep_the_highlight_on_screen() {
    let mut config = soul_sever::config::Config::default();
    config.shares.public = (0..30)
        .map(|index| std::path::PathBuf::from(format!("share-{index:02}")))
        .collect();
    let mut app = App::blank();
    app.bind_config(std::path::PathBuf::from("unused.toml"), config);
    app.set_view(View::Shares);
    let text = render(&app, 120, 40);
    assert!(text.contains("share-00"), "{text}");
    assert!(!text.contains("share-29"), "{text}");
    assert!(text.contains('█'), "{text}");
    press(&mut app, KeyCode::Char('['));
    let text = render(&app, 120, 40);
    assert!(text.contains("share-29"), "{text}");
    assert!(!text.contains("share-00"), "{text}");

    let mut app = App::blank();
    app.set_view(View::Settings);
    for _ in 0..5 {
        press(&mut app, KeyCode::Char('j'));
    }
    let text = render(&app, 70, 18);
    assert!(text.contains("auto-away minutes"), "{text}");
    assert!(!text.contains("replace words"), "{text}");
    press(&mut app, KeyCode::Char('['));
    let text = render(&app, 70, 18);
    assert!(text.contains("replace words"), "{text}");
    assert!(!text.contains("auto-away minutes"), "{text}");
    assert!(text.contains('█'), "{text}");
}

fn settle_library(app: &mut App) {
    let started = std::time::Instant::now();
    while app.library_scan_pending() && started.elapsed() < std::time::Duration::from_secs(2) {
        app.poll_library();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!app.library_scan_pending());
}

fn wait_until_ready(app: &App) {
    let ready = std::time::Instant::now();
    while !app.player_ready() && ready.elapsed() < std::time::Duration::from_secs(2) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(app.player_ready());
}

fn write_tagged(path: &std::path::Path, artist: &str, album: &str, title: &str, track: u32) {
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
    for sample in &samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{Accessor, Tag, TagType};
    let mut file = lofty::read_from_path(path).unwrap();
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_artist(artist.to_owned());
    tag.set_album(album.to_owned());
    tag.set_title(title.to_owned());
    tag.set_track(track);
    file.insert_tag(tag);
    file.save_to_path(path, WriteOptions::default()).unwrap();
}
