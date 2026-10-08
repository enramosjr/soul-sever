//! Soul Sever terminal entry point.

use std::io::{self, stdout};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;

use soul_sever::cli::{self, Launch, Mode};
use soul_sever::config::Config;
use soul_sever::session::{Session, SessionEvent};
use soul_sever::ui::{self, Pointer};
use soul_sever::{App, ScrollId, Target, View};

fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        Ok(Mode::Help) => {
            let _ = cli::write_help(&mut stdout());
            ExitCode::SUCCESS
        }
        Ok(Mode::Version) => {
            println!("soul-sever {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Mode::Run(invocation)) => match cli::load(&invocation) {
            Ok(Launch::Terminal { path, config }) => match run(path, config) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("soul-sever: {err}");
                    ExitCode::from(1)
                }
            },
            Ok(Launch::Rescan(config)) => cli::rescan(&config),
            Ok(Launch::Headless(config)) => cli::headless(config, &mut io::stderr()),
            Err(err) => cli::report_config(err),
        },
        Err(err) => cli::report_usage(err),
    }
}

struct Online {
    runtime: tokio::runtime::Runtime,
    session: Session,
    events: tokio::sync::mpsc::Receiver<SessionEvent>,
}

fn run(path: std::path::PathBuf, config: Config) -> io::Result<()> {
    let mut online = start_session(&config)?;
    let mut app = if config.username.is_empty() {
        App::sign_in()
    } else {
        App::connecting(&config.username)
    };
    app.bind_config(path, config);
    let mut terminal = ratatui::try_init()?;
    execute!(stdout(), EnableMouseCapture)?;
    let mouse = MouseGuard;
    let result = event_loop(&mut terminal, &mut app, &mut online);
    drop(mouse);
    ratatui::restore();
    if let Some(online) = online {
        drop(online.session);
        drop(online.runtime);
    }
    result
}

fn start_session(config: &Config) -> io::Result<Option<Online>> {
    if config.username.is_empty() {
        return Ok(None);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let session = {
        let _guard = runtime.enter();
        Session::spawn(config.clone(), tx)
    };
    Ok(Some(Online {
        runtime,
        session,
        events: rx,
    }))
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    online: &mut Option<Online>,
) -> io::Result<()> {
    let mut pointer = Pointer::default();
    let mut since_tick = Duration::ZERO;
    let mut paced = Instant::now();
    loop {
        app.poll_library();
        app.poll_quality();
        drain(online, app);
        terminal.draw(|frame| ui::draw(frame, app, &mut pointer))?;
        let input = event::poll(app.frame_wait())?;
        let step = paced.elapsed();
        paced = Instant::now();
        app.advance_playback(step);
        if input {
            loop {
                match event::read()? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        app.on_key(key);
                        after_input(online, app)?;
                    }
                    Event::Mouse(mouse) => {
                        apply_mouse(app, &pointer, mouse);
                        after_input(online, app)?;
                    }
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        } else {
            since_tick += step;
            if since_tick >= Duration::from_millis(200) {
                drain(online, app);
                app.on_tick();
                after_input(online, app)?;
                since_tick = Duration::ZERO;
            }
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

fn after_input(online: &mut Option<Online>, app: &mut App) -> io::Result<()> {
    try_sign_in(online, app)?;
    send_search(online, app);
    send_quality_probe(online, app);
    send_download(online, app);
    send_unavailable(online, app);
    send_cancel(online, app);
    send_clear_finished(online, app);
    send_folder(online, app);
    send_browse(online, app);
    send_lists(online, app);
    send_shares(online, app);
    send_forget(online, app);
    send_moves(online, app);
    send_prefs(online, app);
    send_say(online, app);
    send_join(online, app);
    send_inspect(online, app);
    send_presence(online, app);
    Ok(())
}

fn try_sign_in(online: &mut Option<Online>, app: &mut App) -> io::Result<()> {
    let Some(config) = app.take_sign_in() else {
        return Ok(());
    };
    if let Some(old) = online.take() {
        drop(old.session);
        drop(old.runtime);
    }
    *online = start_session(&config)?;
    Ok(())
}

fn send_say(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some((room, text)) = app.take_say() {
        online.runtime.block_on(online.session.say(room, text));
    }
}

fn send_join(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(room) = app.take_join() {
        online.runtime.block_on(online.session.join(room));
    }
}

fn send_inspect(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(user) = app.take_inspect() {
        online.runtime.block_on(online.session.inspect(user));
    }
}

fn send_presence(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(away) = app.take_presence() {
        online.runtime.block_on(online.session.set_away(away));
    }
}

fn send_browse(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(user) = app.take_browse() {
        online.runtime.block_on(online.session.browse(user));
    }
}

fn send_lists(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some((banned, ignored, prioritized)) = app.take_lists() {
        online
            .runtime
            .block_on(online.session.set_lists(banned, ignored, prioritized));
    }
}

fn send_forget(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    let paths = app.take_forget();
    if paths.is_empty() {
        return;
    }
    online.runtime.block_on(online.session.forget(paths));
}

fn send_moves(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    let moves = app.take_moves();
    if moves.is_empty() {
        return;
    }
    online.runtime.block_on(online.session.relocate(moves));
}

fn send_shares(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(shares) = app.take_rescan() {
        online.runtime.block_on(online.session.rescan(shares));
    }
}

fn send_prefs(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(prefs) = app.take_prefs() {
        online.runtime.block_on(online.session.apply_prefs(prefs));
    }
}

fn send_unavailable(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    while let Some((user, album)) = app.take_unavailable() {
        online
            .runtime
            .block_on(online.session.album_unavailable(user, album));
    }
    while let Some((user, path)) = app.take_file_unavailable() {
        online
            .runtime
            .block_on(online.session.file_unavailable(user, path));
    }
}

fn send_quality_probe(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(query) = app.take_quality_probe() {
        online
            .runtime
            .block_on(online.session.quality_search(query));
    }
}

fn send_download(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    while let Some(download) = app.take_download() {
        online.runtime.block_on(online.session.download(download));
    }
}

fn send_clear_finished(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some((downloads, uploads)) = app.take_clear_finished() {
        online
            .runtime
            .block_on(online.session.clear_finished(downloads, uploads));
    }
}

fn send_cancel(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    while let Some((direction, user, path)) = app.take_cancel() {
        online
            .runtime
            .block_on(online.session.cancel(direction, user, path));
    }
}

fn send_folder(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    while let Some((user, directory)) = app.take_folder() {
        online
            .runtime
            .block_on(online.session.folder(user, directory));
    }
}

fn send_search(online: &mut Option<Online>, app: &mut App) {
    let Some(online) = online.as_mut() else {
        return;
    };
    if let Some(search) = app.take_search() {
        online.runtime.block_on(online.session.search(search));
    }
}

fn drain(online: &mut Option<Online>, app: &mut App) {
    if let Some(online) = online.as_mut() {
        while let Ok(event) = online.events.try_recv() {
            app.apply_session(event);
        }
    }
}

fn apply_mouse(app: &mut App, pointer: &Pointer, mouse: MouseEvent) {
    let target = pointer.at(mouse.column, mouse.row);
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.help() {
                app.on_pointer(Target::OpenHelp);
            } else if app.delete_prompt().is_some() {
                app.end_resize();
                app.on_pointer(target.unwrap_or(Target::Scroll));
            } else if let Some(Target::Resize(split, edge)) = target {
                if app.picker_open() || app.signing_in() {
                    app.on_pointer(Target::Resize(split, edge));
                } else {
                    app.begin_resize(split, edge, mouse.column, mouse.row);
                }
            } else if let Some(Target::Seek) = target {
                app.end_resize();
                seek_playback(app, pointer, mouse.column);
            } else if let Some(Target::VScroll(id)) = target {
                app.end_resize();
                if let Some(metrics) = pointer.metrics(id) {
                    app.begin_scroll(id, mouse.row, metrics);
                }
            } else if let Some(target) = target.filter(|target| *target != Target::Scroll) {
                app.end_resize();
                app.on_pointer(target);
            } else {
                app.end_resize();
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.delete_prompt().is_some() {
                return;
            }
            if app.resize_to(mouse.column, mouse.row) {
                return;
            }
            if app.drag_scroll(mouse.row) {
                return;
            }
            if let Some(Target::Seek) = target {
                seek_playback(app, pointer, mouse.column);
            } else if let Some(Target::Select(index)) = target {
                app.select_row(index);
            } else if let Some(Target::Person(index)) = target {
                if !app.person_open() {
                    app.highlight_member(index);
                }
            } else if let Some(Target::Column(column, index)) = target {
                app.focus_library(column, index);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.end_resize();
            app.end_scroll();
        }
        MouseEventKind::ScrollDown => scroll_wheel(app, pointer, target, true),
        MouseEventKind::ScrollUp => scroll_wheel(app, pointer, target, false),
        _ => {}
    }
}

fn scroll_wheel(app: &mut App, pointer: &Pointer, target: Option<Target>, down: bool) {
    if app.help() || app.delete_prompt().is_some() || app.signing_in() {
        return;
    }
    let Some(id) = wheel_scroll(app, target) else {
        return;
    };
    app.scroll_by(id, down, pointer.metrics(id));
}

/// The list whose window the wheel moves. Selection stays on its own keys and clicks.
fn wheel_scroll(app: &App, target: Option<Target>) -> Option<ScrollId> {
    match target? {
        Target::Person(_) if !app.person_open() => Some(ScrollId::Members),
        Target::Person(_) => None,
        Target::VScroll(id) | Target::Wheel(id) => Some(id),
        Target::Picker(_) if app.picker_open() => Some(ScrollId::Picker),
        Target::Picker(_) if app.bind_picker_open() => Some(ScrollId::Bind),
        Target::Setting(_) => Some(ScrollId::Settings),
        Target::Column(column, _) => Some(ScrollId::Library(column)),
        Target::Quality(column, _) => Some(ScrollId::Quality(column)),
        Target::Select(index) if app.view() == View::Shares => {
            Some(ScrollId::Share(u8::try_from(index).unwrap_or(0).min(2)))
        }
        Target::Select(_) | Target::Scroll => Some(ScrollId::List),
        _ => None,
    }
}

fn seek_playback(app: &mut App, pointer: &Pointer, column: u16) {
    let Some(area) = pointer.region(|target| target == Target::Seek) else {
        return;
    };
    app.seek_column(column, area.x, area.width);
}

struct MouseGuard;

impl Drop for MouseGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableMouseCapture);
    }
}
