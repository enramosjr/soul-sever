//! Downloaded music, filed by tags as artist, then album, then song.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};

use crate::quality::Quality;

/// One song in the download folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Song {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub track: u32,
    pub path: PathBuf,
}

impl Song {
    pub fn label(&self) -> String {
        if self.track > 0 {
            format!("{:02} {}", self.track, self.title)
        } else {
            self.title.clone()
        }
    }
}

/// An album's songs, in track order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Album {
    pub name: String,
    pub songs: Vec<Song>,
}

/// An artist's albums.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artist {
    pub name: String,
    pub albums: Vec<Album>,
}

/// The download folder, grouped for the library screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub artists: Vec<Artist>,
    filed: Vec<Filed>,
}

/// A file the library scan moved into its album-artist folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub root: PathBuf,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// A song kept from an earlier pass, with the file stamp that made the tags valid.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Filed {
    song: Song,
    stamp: Stamp,
    quality: Option<Quality>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Stamp {
    modified_secs: Option<u64>,
    modified_nanos: Option<u32>,
    len: u64,
}

impl Catalog {
    pub fn scan(root: &Path) -> Self {
        Self::default().reconcile(root).0
    }

    /// Artists and albums taken from the two folders above each file, with
    /// guest wording removed. No tags are read and nothing is renamed, so the
    /// library can be drawn before the full scan rewrites files. `skip` is left
    /// unread, so a staged replacement inside the incomplete folder is not listed.
    pub(crate) fn preview_skipping(root: &Path, skip: Option<&Path>) -> Self {
        let mut paths = Vec::new();
        list_audio(root, skip, &mut paths);
        let mut songs = Vec::new();
        for path in paths {
            if let Some(song) = from_folders(root, &path) {
                songs.push(song);
            }
        }
        align_spellings(&mut songs);
        group(songs)
    }

    /// Writes the catalog next to the account file. A later launch can show
    /// this list before the music folder is read.
    pub fn store(&self, path: &Path, root: &Path) {
        if !root.is_dir() {
            return;
        }
        let saved = SavedLibrary {
            root: root.to_string_lossy().into_owned(),
            songs: self
                .filed
                .iter()
                .map(|filed| SavedSong {
                    artist: filed.song.artist.clone(),
                    album: filed.song.album.clone(),
                    title: filed.song.title.clone(),
                    track: filed.song.track,
                    path: filed.song.path.to_string_lossy().into_owned(),
                    modified_secs: filed.stamp.modified_secs,
                    modified_nanos: filed.stamp.modified_nanos,
                    len: filed.stamp.len,
                    quality: filed.quality.clone(),
                })
                .collect(),
        };
        let Ok(text) = toml::to_string(&saved) else {
            return;
        };
        let tmp = path.with_extension("toml.tmp");
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }

    /// The catalog saved for `root`. A missing file, a different folder, or a
    /// file that does not parse is ignored.
    pub fn load(path: &Path, root: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        let saved: SavedLibrary = toml::from_str(&text).ok()?;
        if saved.root != root.to_string_lossy() {
            return None;
        }
        let songs: Vec<Song> = saved
            .songs
            .iter()
            .map(|song| Song {
                artist: song.artist.clone(),
                album: song.album.clone(),
                title: song.title.clone(),
                track: song.track,
                path: PathBuf::from(&song.path),
            })
            .collect();
        let filed = saved
            .songs
            .iter()
            .map(|song| Filed {
                song: Song {
                    artist: song.artist.clone(),
                    album: song.album.clone(),
                    title: song.title.clone(),
                    track: song.track,
                    path: PathBuf::from(&song.path),
                },
                stamp: Stamp {
                    modified_secs: song.modified_secs,
                    modified_nanos: song.modified_nanos,
                    len: song.len,
                },
                quality: song.quality.clone(),
            })
            .collect();
        let mut catalog = group(songs);
        catalog.filed = filed;
        Some(catalog)
    }

    /// Keeps songs whose files are unchanged. Tags are read for a file that
    /// arrived or was rewritten, and a file that is gone leaves the list.
    /// A song whose album artist folder differs is renamed, and that move is
    /// returned beside the catalog.
    pub fn reconcile(&self, root: &Path) -> (Self, Vec<Relocation>) {
        self.reconcile_skipping(root, None)
    }

    /// Same as [`Self::reconcile`], leaving `skip` unread.
    pub fn reconcile_skipping(&self, root: &Path, skip: Option<&Path>) -> (Self, Vec<Relocation>) {
        let mut paths = Vec::new();
        list_audio(root, skip, &mut paths);
        let mut known: BTreeMap<PathBuf, (Song, Stamp, Option<Quality>)> = self
            .filed
            .iter()
            .map(|filed| {
                (
                    filed.song.path.clone(),
                    (filed.song.clone(), filed.stamp, filed.quality.clone()),
                )
            })
            .collect();

        let mut songs = Vec::new();
        let mut carried: BTreeMap<PathBuf, Quality> = BTreeMap::new();
        let mut fresh = BTreeSet::new();
        for path in paths {
            let stamp = file_stamp(&path);
            if stamp.modified_secs.is_some()
                && let Some((song, old, quality)) = known.remove(&path)
                && old == stamp
            {
                if let Some(quality) = quality {
                    carried.insert(path, quality);
                }
                songs.push(song);
                continue;
            }
            if let Some(song) = identify(root, &path) {
                fresh.insert(path);
                songs.push(song);
            }
        }

        let prior: Vec<(PathBuf, String, String)> = songs
            .iter()
            .map(|song| (song.path.clone(), song.artist.clone(), song.album.clone()))
            .collect();
        align_spellings(&mut songs);

        let mut filed = Vec::with_capacity(songs.len());
        let mut moved = Vec::new();
        for song in &mut songs {
            let renamed = prior.iter().any(|(path, artist, album)| {
                path == &song.path && (artist != &song.artist || album != &song.album)
            });
            if fresh.contains(&song.path) || renamed {
                store_names(&song.path, &song.artist, &song.album);
            }
            let quality = carried.remove(&song.path);
            if let Some(relocation) = relocate(root, song) {
                moved.push(relocation);
            }
            filed.push(Filed {
                stamp: file_stamp(&song.path),
                song: song.clone(),
                quality,
            });
        }
        let mut catalog = group(songs);
        catalog.filed = filed;
        (catalog, moved)
    }

    pub(crate) fn forget(&mut self, paths: &[PathBuf]) {
        self.filed
            .retain(|filed| !paths.iter().any(|path| path == &filed.song.path));
        for artist in &mut self.artists {
            for album in &mut artist.albums {
                album
                    .songs
                    .retain(|song| !paths.iter().any(|path| path == &song.path));
            }
            artist.albums.retain(|album| !album.songs.is_empty());
        }
        self.artists.retain(|artist| !artist.albums.is_empty());
    }

    /// Records a measurement for a song already in the catalog. An unknown path
    /// is ignored, so a worker that finishes after a delete does not resurrect it.
    pub fn set_quality(&mut self, path: &Path, quality: Quality) -> bool {
        let Some(filed) = self.filed.iter_mut().find(|filed| filed.song.path == path) else {
            return false;
        };
        filed.quality = Some(quality);
        true
    }

    pub fn song(&self, path: &Path) -> Option<&Song> {
        self.filed
            .iter()
            .find(|filed| filed.song.path == path)
            .map(|filed| &filed.song)
    }

    pub fn quality(&self, path: &Path) -> Option<&Quality> {
        self.filed
            .iter()
            .find(|filed| filed.song.path == path)
            .and_then(|filed| filed.quality.as_ref())
    }

    /// Remembers that a replacement search already ran for this file stamp.
    pub fn mark_upgrade_attempted(&mut self, path: &Path) -> bool {
        let Some(quality) = self
            .filed
            .iter_mut()
            .find(|filed| filed.song.path == path)
            .and_then(|filed| filed.quality.as_mut())
        else {
            return false;
        };
        quality.upgrade_attempted = true;
        true
    }

    /// Drops every saved verdict so the library can be measured again.
    pub fn forget_quality(&mut self) {
        for filed in &mut self.filed {
            filed.quality = None;
        }
    }

    /// Songs with no measurement for the current stamp, except `playing`.
    pub fn pending_analysis(&self, playing: &Path) -> Vec<PathBuf> {
        self.filed
            .iter()
            .filter(|filed| filed.quality.is_none() && filed.song.path != playing)
            .map(|filed| filed.song.path.clone())
            .collect()
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = (&Song, Option<&Quality>)> {
        self.filed
            .iter()
            .map(|filed| (&filed.song, filed.quality.as_ref()))
    }

    /// The first song that still needs a replacement search.
    pub fn pending_upgrade(&self) -> Option<&Song> {
        self.filed.iter().find_map(|filed| {
            filed
                .quality
                .as_ref()
                .and_then(|quality| quality.goal().map(|_| &filed.song))
        })
    }
}

fn group(songs: Vec<Song>) -> Catalog {
    let mut artists: Vec<Artist> = Vec::new();
    for song in songs {
        let artist = artists.iter_mut().find(|artist| artist.name == song.artist);
        let artist = if let Some(artist) = artist {
            artist
        } else {
            artists.push(Artist {
                name: song.artist.clone(),
                albums: Vec::new(),
            });
            artists.last_mut().unwrap()
        };
        let album = artist
            .albums
            .iter_mut()
            .find(|album| album.name == song.album);
        if let Some(album) = album {
            album.songs.push(song);
        } else {
            artist.albums.push(Album {
                name: song.album.clone(),
                songs: vec![song],
            });
        }
    }
    for artist in &mut artists {
        artist
            .albums
            .sort_by(|left, right| cmp_name(&left.name, &right.name));
        for album in &mut artist.albums {
            album.songs.sort_by(|left, right| {
                track_key(left.track)
                    .cmp(&track_key(right.track))
                    .then_with(|| cmp_name(&left.title, &right.title))
            });
        }
    }
    artists.sort_by(|left, right| cmp_name(&left.name, &right.name));
    Catalog {
        artists,
        filed: Vec::new(),
    }
}

struct Tags {
    artist: Option<String>,
    album_artist: Option<String>,
    album: Option<String>,
    title: Option<String>,
    track: u32,
}

/// Where a finished audio file belongs. `None` keeps the Soulseek folder layout.
pub fn shelf(root: &Path, source: &Path) -> Option<PathBuf> {
    if !is_audio(source) {
        return None;
    }
    let tags = read_tags(source);
    if tags.artist.is_none()
        && tags.album_artist.is_none()
        && tags.album.is_none()
        && tags.title.is_none()
    {
        return None;
    }
    let artist = sanitize(&vote_artist(&tags).unwrap_or_else(|| "Unknown Artist".to_owned()));
    let album = sanitize(&vote_album(&tags).unwrap_or_else(|| "Unknown Album".to_owned()));
    let (artist, album) = match_library(root, source, &artist, &album);
    store_names(source, &artist, &album);
    let title = sanitize(&tags.title.unwrap_or_else(|| stem(source)));
    let ext = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("audio");
    let file = if tags.track > 0 {
        format!("{:02} {title}.{ext}", tags.track)
    } else {
        format!("{title}.{ext}")
    };
    let file = sanitize(&file);
    Some(vacant(root.join(artist).join(album).join(file)))
}

/// Where a replacement of `original` belongs. The new file keeps that song's
/// artist, album, title, and track, so its own tags cannot open a second artist.
pub(crate) fn upgrade_destination(root: &Path, original: &Path, staged: &Path) -> Option<PathBuf> {
    if !is_audio(staged) {
        return None;
    }
    let Some(song) = identify(root, original).or_else(|| from_folders(root, original)) else {
        return shelf(root, staged);
    };
    let (artist, album) = match_library(root, staged, &song.artist, &song.album);
    let title = sanitize(&song.title);
    let ext = staged
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("audio");
    write_tags(
        staged,
        &artist,
        &album,
        Some(title.as_str()),
        (song.track > 0).then_some(song.track),
    );
    let file = if song.track > 0 {
        format!("{:02} {title}.{ext}", song.track)
    } else {
        format!("{title}.{ext}")
    };
    let file = sanitize(&file);
    Some(vacant_except(
        root.join(sanitize(&artist))
            .join(sanitize(&album))
            .join(file),
        original,
    ))
}

/// After songs have been filed, drop leftover files and empty folders under `root`.
///
/// Audio stays. `root` stays. `skip` is left untouched, so an incomplete folder
/// nested inside the music folder is not cleared. Images and other non-audio
/// files are removed, including ones sitting beside a song, and a directory
/// that no longer holds audio is removed. The returned paths are the files
/// that were deleted.
pub fn sweep(root: &Path, skip: Option<&Path>) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if root.as_os_str().is_empty() {
        return removed;
    }
    let _ = sweep_dir(root, root, skip, &mut removed);
    removed
}

/// `true` when `dir` still holds audio, or is the incomplete folder.
fn sweep_dir(root: &Path, dir: &Path, skip: Option<&Path>, removed: &mut Vec<PathBuf>) -> bool {
    if skip.is_some_and(|skip| dir == skip) {
        return true;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut holds = false;
    let mut child_dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path == root || !path.starts_with(root) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            if fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
            continue;
        }
        if meta.is_dir() {
            child_dirs.push(path);
        } else {
            files.push(path);
        }
    }
    for path in child_dirs {
        if sweep_dir(root, &path, skip, removed) {
            holds = true;
        } else {
            let _ = fs::remove_dir(&path);
        }
    }
    for path in files {
        if is_audio(&path) {
            holds = true;
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    holds
}

/// Deletes `paths` that sit under `root`, then removes emptied album and artist folders.
pub fn discard(root: &Path, paths: &[PathBuf]) {
    for path in paths {
        if !path.starts_with(root) || path == root {
            continue;
        }
        let _ = fs::remove_file(path);
        remove_empty_parents(root, path);
    }
}

fn remove_empty_parents(root: &Path, path: &Path) {
    let mut dir = path.parent();
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            break;
        }
        if fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

fn list_audio(dir: &Path, skip: Option<&Path>, paths: &mut Vec<PathBuf>) {
    if skip.is_some_and(|skip| dir == skip) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            list_audio(&path, skip, paths);
        } else if is_audio(&path) {
            paths.push(path);
        }
    }
}

fn file_stamp(path: &Path) -> Stamp {
    match fs::metadata(path) {
        Ok(meta) => {
            let (modified_secs, modified_nanos) = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                .map(|duration| (Some(duration.as_secs()), Some(duration.subsec_nanos())))
                .unwrap_or((None, None));
            Stamp {
                modified_secs,
                modified_nanos,
                len: meta.len(),
            }
        }
        Err(_) => Stamp {
            modified_secs: None,
            modified_nanos: None,
            len: 0,
        },
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedLibrary {
    root: String,
    songs: Vec<SavedSong>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSong {
    artist: String,
    album: String,
    title: String,
    track: u32,
    path: String,
    modified_secs: Option<u64>,
    modified_nanos: Option<u32>,
    len: u64,
    #[serde(default)]
    quality: Option<Quality>,
}

fn collect(root: &Path, dir: &Path, songs: &mut Vec<Song>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, songs);
            continue;
        }
        if let Some(song) = identify(root, &path) {
            songs.push(song);
        }
    }
}

fn identify(root: &Path, path: &Path) -> Option<Song> {
    if !is_audio(path) {
        return None;
    }
    let tags = read_tags(path);
    let (artist_from_path, album_from_path, file) = path_credit(root, path)?;
    let title = tags.title.clone().unwrap_or_else(|| stem_str(&file));
    Some(Song {
        artist: sanitize(&vote_artist(&tags).unwrap_or_else(|| primary_credit(&artist_from_path))),
        album: sanitize(&vote_album(&tags).unwrap_or_else(|| primary_credit(&album_from_path))),
        title,
        track: tags.track,
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
thread_local! {
    static TAG_READS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn note_tag_read() {
    #[cfg(test)]
    TAG_READS.with(|reads| {
        if let Some(count) = reads.get() {
            reads.set(Some(count + 1));
        }
    });
}

fn read_tags(path: &Path) -> Tags {
    note_tag_read();
    let Ok(tagged) = lofty::read_from_path(path) else {
        return Tags::empty();
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Tags::empty();
    };
    Tags {
        artist: cleaned(tag.artist().as_deref()),
        album_artist: cleaned(tag.get_string(ItemKey::AlbumArtist)),
        album: cleaned(tag.album().as_deref()),
        title: cleaned(tag.title().as_deref()),
        track: tag.track().unwrap_or(0),
    }
}

impl Tags {
    fn empty() -> Self {
        Self {
            artist: None,
            album_artist: None,
            album: None,
            title: None,
            track: 0,
        }
    }
}

fn from_folders(root: &Path, path: &Path) -> Option<Song> {
    if !is_audio(path) {
        return None;
    }
    let (artist, album, file) = path_credit(root, path)?;
    Some(Song {
        artist: sanitize(&primary_credit(&artist)),
        album: sanitize(&primary_credit(&album)),
        title: stem_str(&file),
        track: 0,
        path: path.to_path_buf(),
    })
}

fn path_credit(root: &Path, path: &Path) -> Option<(String, String, String)> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut parts: Vec<String> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
        .collect();
    let file = parts.pop()?;
    let album = parts.pop().unwrap_or_else(|| "Unknown Album".to_owned());
    let artist = parts.pop().unwrap_or_else(|| "Unknown Artist".to_owned());
    Some((artist, album, file))
}

fn cleaned(value: Option<&str>) -> Option<String> {
    let text = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { None } else { Some(text) }
}

/// The album credit: guest joiners are not part of it. `Drake with X` is
/// `Drake`. `Simon & Garfunkel` stays. A cut that would leave nothing keeps
/// the original text. The song title is left as tagged.
fn primary_credit(name: &str) -> String {
    const GUESTS: &[&str] = &[
        "featuring",
        "versus",
        "feat.",
        "with",
        "feat",
        "ft.",
        "vs.",
        "w/",
        "w.",
        "f/",
        "f.",
        "ft",
        "vs",
        "w",
        "f",
    ];
    let Some(index) = cut_at(name, GUESTS) else {
        return name.to_owned();
    };
    let stripped = trimmed_prefix(name, index);
    if stripped.is_empty() {
        name.to_owned()
    } else {
        stripped
    }
}

/// Joiners that add someone onto an artist who already has their own folder.
/// `and` and `&` stay on a duo such as `Simon & Garfunkel` until that shorter
/// name already exists on another album.
const EXTENSIONS: &[&str] = &[
    "featuring",
    "versus",
    "feat.",
    "with",
    "feat",
    "ft.",
    "vs.",
    "w/",
    "w.",
    "f/",
    "f.",
    "and",
    "ft",
    "vs",
    "&",
    "+",
    "x",
    "w",
    "f",
];

fn cut_at(name: &str, markers: &[&str]) -> Option<usize> {
    for (index, _) in name.char_indices() {
        if !edge_before(name, index) {
            continue;
        }
        let rest = &name[index..];
        if markers
            .iter()
            .any(|marker| starts_with_ignore_ascii(rest, marker) && marker_ends(rest, marker))
        {
            return Some(index);
        }
    }
    None
}

fn trimmed_prefix(name: &str, index: usize) -> String {
    name[..index]
        .trim_end_matches(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '(' | '[' | '-' | '–' | '—' | ',' | '/' | '|' | ':'
                )
        })
        .trim()
        .to_owned()
}

/// Album-artist tag when it is set, otherwise the track artist's primary credit.
fn vote_artist(tags: &Tags) -> Option<String> {
    let tagged = tags
        .album_artist
        .as_deref()
        .map(primary_credit)
        .filter(|name| !name.is_empty());
    if tagged.is_some() {
        return tagged;
    }
    tags.artist
        .as_deref()
        .map(primary_credit)
        .filter(|name| !name.is_empty())
}

fn vote_album(tags: &Tags) -> Option<String> {
    tags.album
        .as_deref()
        .map(primary_credit)
        .filter(|name| !name.is_empty())
}

fn starts_with_ignore_ascii(hay: &str, marker: &str) -> bool {
    let mut hay = hay.chars();
    for expected in marker.chars() {
        match hay.next() {
            Some(got) if got.eq_ignore_ascii_case(&expected) => {}
            _ => return false,
        }
    }
    true
}

fn edge_before(name: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    name[..index].chars().next_back().is_some_and(|character| {
        character.is_whitespace() || matches!(character, '(' | '[' | '-' | '–' | '—' | '/' | '|')
    })
}

fn marker_ends(rest: &str, marker: &str) -> bool {
    match rest[marker.len()..].chars().next() {
        None => true,
        Some(character) => {
            character.is_whitespace()
                || matches!(character, ')' | ']' | ',' | ';' | ':')
                || (marker.ends_with(['.', '/']) && character.is_alphanumeric())
        }
    }
}

/// `shorter` is the start of a collaborative credit when `longer` continues
/// with `&`, `and`, `+`, or a comma. Guest wording is already gone.
fn extends_with_primary(shorter: &str, longer: &str) -> bool {
    let shorter: Vec<char> = shorter.chars().collect();
    let longer: Vec<char> = longer.chars().collect();
    if shorter.is_empty() || longer.len() <= shorter.len() {
        return false;
    }
    if !longer[..shorter.len()]
        .iter()
        .zip(shorter.iter())
        .all(|(left, right)| left.eq_ignore_ascii_case(right))
    {
        return false;
    }
    let next = longer[shorter.len()];
    if !(next.is_whitespace()
        || matches!(
            next,
            '(' | '[' | '-' | '–' | '—' | '/' | '|' | ',' | ':' | '&' | '+'
        ))
    {
        return false;
    }
    let remainder: String = longer[shorter.len()..].iter().collect();
    starts_with_primary_joiner(remainder.trim_start())
}

fn starts_with_primary_joiner(remainder: &str) -> bool {
    const JOINERS: &[&str] = &["and", "&", "+", ","];
    JOINERS
        .iter()
        .any(|joiner| starts_with_ignore_ascii(remainder, joiner) && joiner_ends(remainder, joiner))
}

fn joiner_ends(rest: &str, joiner: &str) -> bool {
    match rest[joiner.len()..].chars().next() {
        None => false,
        Some(character) => {
            if joiner == "and" {
                character.is_whitespace() || matches!(character, '(' | '[')
            } else {
                character.is_whitespace()
                    || character.is_alphanumeric()
                    || matches!(character, '(' | '[')
            }
        }
    }
}

/// Within one album, a shorter credit adopts the longer collaborative credit.
/// `Simon` beside `Simon & Garfunkel` becomes the duo. Unrelated names stay.
fn unify_credits(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| {
            names
                .iter()
                .filter(|other| extends_with_primary(name, other))
                .max_by(|left, right| {
                    left.chars()
                        .count()
                        .cmp(&right.chars().count())
                        .then_with(|| fold_key(left).cmp(&fold_key(right)))
                })
                .cloned()
                .unwrap_or_else(|| name.clone())
        })
        .collect()
}

/// One artist and one album spelling for the whole library.
///
/// Case and lookalike punctuation share a spelling. A guest tacked on with
/// `and`, `&`, `+`, or `x` rolls under the artist who already has another
/// album. A duo that only appears together, such as `Simon & Garfunkel`, stays
/// one name, and a shorter credit on that album joins it.
fn align_spellings(songs: &mut [Song]) {
    spell_artists(songs);
    absorb_featured(songs);
    unify_album_artists(songs);
    spell_artists(songs);
    spell_albums(songs);
}

fn spell_artists(songs: &mut [Song]) {
    let artist_names: Vec<String> = songs.iter().map(|song| song.artist.clone()).collect();
    let artists = spellings(&artist_names);
    for song in songs.iter_mut() {
        if let Some(name) = artists.get(&fold_key(&song.artist)) {
            song.artist.clone_from(name);
        }
    }
}

fn spell_albums(songs: &mut [Song]) {
    let mut albums_by_artist: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for song in songs.iter() {
        albums_by_artist
            .entry(song.artist.clone())
            .or_default()
            .push(song.album.clone());
    }
    let albums: BTreeMap<String, BTreeMap<String, String>> = albums_by_artist
        .into_iter()
        .map(|(artist, names)| (artist, spellings(&names)))
        .collect();
    for song in songs.iter_mut() {
        if let Some(names) = albums.get(&song.artist)
            && let Some(name) = names.get(&fold_key(&song.album))
        {
            song.album.clone_from(name);
        }
    }
}

fn unify_album_artists(songs: &mut [Song]) {
    let mut by_album: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, song) in songs.iter().enumerate() {
        by_album
            .entry(fold_key(&song.album))
            .or_default()
            .push(index);
    }
    for indices in by_album.values() {
        let names: Vec<String> = indices
            .iter()
            .map(|index| songs[*index].artist.clone())
            .collect();
        let unified = unify_credits(&names);
        for (slot, index) in indices.iter().enumerate() {
            songs[*index].artist.clone_from(&unified[slot]);
        }
    }
}

/// `Nine Inch Nails & David Bowie` joins `Nine Inch Nails` when that artist
/// already has a different album. The prefix has to stand on its own, so
/// `Simon` beside `Simon & Garfunkel` on one album does not swallow the duo.
fn absorb_featured(songs: &mut [Song]) {
    let mut albums_for: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut display: BTreeMap<String, String> = BTreeMap::new();
    for song in songs.iter() {
        let key = fold_key(&song.artist);
        albums_for
            .entry(key.clone())
            .or_default()
            .insert(fold_key(&song.album));
        display.entry(key).or_insert_with(|| song.artist.clone());
    }
    for song in songs.iter_mut() {
        let Some(index) = cut_at(&song.artist, EXTENSIONS) else {
            continue;
        };
        let prefix = trimmed_prefix(&song.artist, index);
        let key = fold_key(&prefix);
        if prefix.is_empty() || key == fold_key(&song.artist) {
            continue;
        }
        let Some(albums) = albums_for.get(&key) else {
            continue;
        };
        let here = fold_key(&song.album);
        if albums.iter().any(|album| album != &here)
            && let Some(name) = display.get(&key)
        {
            song.artist.clone_from(name);
        }
    }
}

fn match_library(root: &Path, source: &Path, artist: &str, album: &str) -> (String, String) {
    let mut songs = Vec::new();
    collect(root, root, &mut songs);
    songs.retain(|song| song.path != source);
    let placeholder = source.to_path_buf();
    songs.push(Song {
        artist: artist.to_owned(),
        album: album.to_owned(),
        title: String::new(),
        track: 0,
        path: placeholder.clone(),
    });
    align_spellings(&mut songs);
    songs
        .into_iter()
        .find(|song| song.path == placeholder)
        .map(|song| (song.artist, song.album))
        .unwrap_or_else(|| (artist.to_owned(), album.to_owned()))
}

fn spellings(names: &[String]) -> BTreeMap<String, String> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in names {
        groups.entry(fold_key(name)).or_default().push(name.clone());
    }
    groups
        .into_iter()
        .map(|(key, variants)| (key, choose(&variants)))
        .collect()
}

fn choose(variants: &[String]) -> String {
    let repaired: Vec<String> = variants.iter().map(|name| repair_casing(name)).collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for name in &repaired {
        *counts.entry(name.as_str()).or_default() += 1;
    }
    if counts.len() == 1 {
        return counts
            .into_keys()
            .next()
            .unwrap_or(repaired[0].as_str())
            .to_owned();
    }
    let mixed: Vec<(&str, usize)> = counts
        .iter()
        .filter(|(name, _)| mixed_case(name))
        .map(|(name, count)| (*name, *count))
        .collect();
    if mixed.is_empty() {
        return title_case(&fold_key(&repaired[0]));
    }
    mixed
        .into_iter()
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then(lowercase_letters(left.0).cmp(&lowercase_letters(right.0)))
                .then(ascii_letters(left.0).cmp(&ascii_letters(right.0)))
                .then_with(|| right.0.cmp(left.0))
        })
        .map(|(name, _)| name.to_owned())
        .unwrap_or_else(|| repaired[0].clone())
}

fn store_names(path: &Path, artist: &str, album: &str) {
    write_tags(path, artist, album, None, None);
}

fn write_tags(path: &Path, artist: &str, album: &str, title: Option<&str>, track: Option<u32>) {
    note_tag_read();
    let Ok(mut file) = lofty::read_from_path(path) else {
        return;
    };
    let primary = file.primary_tag().is_some();
    let Some(tag) = (if primary {
        file.primary_tag_mut()
    } else {
        file.first_tag_mut()
    }) else {
        return;
    };
    let current_artist = tag.artist().map(|value| value.into_owned());
    let current_album = tag.album().map(|value| value.into_owned());
    let current_album_artist = tag.get_string(ItemKey::AlbumArtist).map(str::to_owned);
    let current_title = tag.title().map(|value| value.into_owned());
    let mut changed = false;
    if current_artist.as_deref() != Some(artist) {
        tag.set_artist(artist.to_owned());
        changed = true;
    }
    if current_album.is_some() && current_album.as_deref() != Some(album) {
        tag.set_album(album.to_owned());
        changed = true;
    }
    if current_album_artist.as_deref() != Some(artist) {
        tag.insert_text(ItemKey::AlbumArtist, artist.to_owned());
        changed = true;
    }
    if let Some(title) = title
        && current_title.as_deref() != Some(title)
    {
        tag.set_title(title.to_owned());
        changed = true;
    }
    if let Some(track) = track
        && tag.track() != Some(track)
    {
        tag.set_track(track);
        changed = true;
    }
    if changed {
        let _ = file.save_to_path(path, WriteOptions::default());
    }
}

/// Apostrophes and dashes that look alike are the same character, then case.
fn fold_key(name: &str) -> String {
    name.chars()
        .map(|character| match character {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '`' | '\u{00B4}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => '-',
            other => other,
        })
        .collect::<String>()
        .to_lowercase()
}

fn mixed_case(name: &str) -> bool {
    let mut upper = false;
    let mut lower = false;
    for character in name.chars() {
        upper |= character.is_uppercase();
        lower |= character.is_lowercase();
    }
    upper && lower
}

fn lowercase_letters(name: &str) -> usize {
    name.chars()
        .filter(|character| character.is_lowercase())
        .count()
}

fn ascii_letters(name: &str) -> usize {
    name.chars()
        .filter(|character| character.is_ascii())
        .count()
}

/// A mixed-case name capitalizes a fully lowercase word. `Nine inch Nails`
/// becomes `Nine Inch Nails`. A small word in the middle stays lowercase, and
/// a single letter such as `x` stays put. All capitals or all lowercase is
/// left for the uniform-case rule.
fn repair_casing(name: &str) -> String {
    if !mixed_case(name) {
        return name.to_owned();
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    let last = words.len().saturating_sub(1);
    words
        .into_iter()
        .enumerate()
        .map(|(index, word)| {
            let all_lower = word.chars().all(|character| !character.is_uppercase());
            let single = word.chars().count() == 1;
            if all_lower && !single && !(index != 0 && index != last && small_word(word)) {
                capitalize(word)
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let last = words.len().saturating_sub(1);
    words
        .into_iter()
        .enumerate()
        .map(|(index, word)| {
            if index != 0 && index != last && small_word(word) {
                word.to_owned()
            } else {
                capitalize(word)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn small_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "at"
            | "but"
            | "by"
            | "for"
            | "in"
            | "nor"
            | "of"
            | "on"
            | "or"
            | "the"
            | "to"
            | "via"
            | "vs"
    )
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out: String = first.to_uppercase().collect();
    for character in chars {
        out.extend(character.to_lowercase());
    }
    out
}

fn relocate(root: &Path, song: &mut Song) -> Option<Relocation> {
    let dest = filed_dest(root, song)?;
    if dest == song.path {
        return None;
    }
    let parent = dest.parent()?;
    fs::create_dir_all(parent).ok()?;
    if fs::rename(&song.path, &dest).is_err() {
        return None;
    }
    let from = std::mem::replace(&mut song.path, dest.clone());
    remove_empty_parents(root, &from);
    Some(Relocation {
        root: root.to_path_buf(),
        from,
        to: dest,
    })
}

fn filed_dest(root: &Path, song: &Song) -> Option<PathBuf> {
    if root.as_os_str().is_empty() || !song.path.starts_with(root) {
        return None;
    }
    let file = song.path.file_name()?;
    let dest = root
        .join(sanitize(&song.artist))
        .join(sanitize(&song.album))
        .join(file);
    if dest == song.path {
        Some(dest)
    } else {
        Some(vacant(dest))
    }
}

fn vacant(path: PathBuf) -> PathBuf {
    vacant_except(path, Path::new(""))
}

fn vacant_except(path: PathBuf, except: &Path) -> PathBuf {
    if !path.exists() || path == except {
        return path;
    }
    let parent = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("song");
    let ext = path.extension().and_then(|ext| ext.to_str());
    for number in 2..1_000 {
        let name = match ext {
            Some(ext) => format!("{stem} ({number}).{ext}"),
            None => format!("{stem} ({number})"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

fn sanitize(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|character| match character {
            '/' | '\\' | '\0' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect();
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = cleaned.trim_matches('.').trim();
    if trimmed.is_empty() || trimmed == ".." {
        "Unknown".to_owned()
    } else {
        trimmed.chars().take(120).collect()
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Untitled")
        .to_owned()
}

fn stem_str(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file)
        .to_owned()
}

fn is_audio(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "aac"
            | "aif"
            | "aiff"
            | "ape"
            | "flac"
            | "m4a"
            | "mp3"
            | "mp4"
            | "ogg"
            | "opus"
            | "wav"
            | "wma"
    )
}

fn cmp_name(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase().cmp(&right.to_lowercase())
}

fn track_key(track: u32) -> u32 {
    if track == 0 { u32::MAX } else { track }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{ItemKey, Tag, TagType};

    #[test]
    fn a_finished_song_is_filed_under_its_artist_and_album() {
        let root = scratch("shelf");
        let loose = root.join("incoming");
        fs::create_dir_all(&loose).unwrap();
        let first = loose.join("basket.wav");
        let second = loose.join("nice.wav");
        let third = loose.join("when.wav");
        write_tone(&first);
        write_tone(&second);
        write_tone(&third);
        tag(&first, "Green Day", "Dookie", "Basket Case", 2);
        tag(&second, "Green Day", "Nimrod", "Nice Guys Finish Last", 1);
        tag(&third, "Green Day", "Dookie", "When I Come Around", 1);

        let basket = shelf(&root, &first).unwrap();
        let nice = shelf(&root, &second).unwrap();
        let when = shelf(&root, &third).unwrap();
        fs::create_dir_all(basket.parent().unwrap()).unwrap();
        fs::create_dir_all(nice.parent().unwrap()).unwrap();
        fs::create_dir_all(when.parent().unwrap()).unwrap();
        fs::rename(&first, &basket).unwrap();
        fs::rename(&second, &nice).unwrap();
        fs::rename(&third, &when).unwrap();

        assert_eq!(
            basket,
            root.join("Green Day")
                .join("Dookie")
                .join("02 Basket Case.wav")
        );
        assert_eq!(
            nice,
            root.join("Green Day")
                .join("Nimrod")
                .join("01 Nice Guys Finish Last.wav")
        );
        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Green Day");
        assert_eq!(catalog.artists[0].albums.len(), 2);
        assert_eq!(catalog.artists[0].albums[0].name, "Dookie");
        assert_eq!(
            catalog.artists[0].albums[0].songs[0].label(),
            "01 When I Come Around"
        );
        assert_eq!(
            catalog.artists[0].albums[0].songs[1].label(),
            "02 Basket Case"
        );
        assert_eq!(catalog.artists[0].albums[1].name, "Nimrod");
        assert_eq!(catalog.artists[0].albums[1].songs.len(), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_without_tags_keeps_its_folder() {
        let root = scratch("plain");
        let source = root.join("tone.wav");
        write_tone(&source);
        assert!(shelf(&root, &source).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn case_variants_of_an_artist_and_album_become_one_name() {
        let root = scratch("case");
        let first = root.join("falling.wav");
        let second = root.join("rolling.wav");
        write_tone(&first);
        write_tone(&second);
        tag(&first, "Hail the Sun", "Wake", "Falling on Deaf Ears", 1);
        tag(
            &second,
            "Hail The Sun",
            "wake",
            "Rolling Out the Welcome Mat",
            2,
        );

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Hail the Sun");
        assert_eq!(catalog.artists[0].albums.len(), 1);
        assert_eq!(catalog.artists[0].albums[0].name, "Wake");
        assert_eq!(catalog.artists[0].albums[0].songs.len(), 2);
        let songs = &catalog.artists[0].albums[0].songs;
        assert_eq!(
            read_tags(&songs[0].path).artist.as_deref(),
            Some("Hail the Sun")
        );
        assert_eq!(
            read_tags(&songs[1].path).artist.as_deref(),
            Some("Hail the Sun")
        );
        assert_eq!(read_tags(&songs[0].path).album.as_deref(), Some("Wake"));
        assert_eq!(read_tags(&songs[1].path).album.as_deref(), Some("Wake"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_most_common_spelling_wins_when_the_casings_disagree() {
        let root = scratch("majority");
        let paths: Vec<PathBuf> = (0..3)
            .map(|index| root.join(format!("song{index}.wav")))
            .collect();
        for path in &paths {
            write_tone(path);
        }
        tag(&paths[0], "Hail The Sun", "Wake", "One", 1);
        tag(&paths[1], "Hail The Sun", "Wake", "Two", 2);
        tag(&paths[2], "Hail the Sun", "Wake", "Three", 3);

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Hail The Sun");
        let three = catalog.artists[0].albums[0]
            .songs
            .iter()
            .find(|song| song.title == "Three")
            .unwrap();
        assert_eq!(
            read_tags(&three.path).artist.as_deref(),
            Some("Hail The Sun")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn all_caps_and_all_lower_become_title_case_and_a_lone_name_stays() {
        let root = scratch("uniform");
        let loud = root.join("loud.wav");
        let quiet = root.join("quiet.wav");
        let alone = root.join("alone.wav");
        write_tone(&loud);
        write_tone(&quiet);
        write_tone(&alone);
        tag(&loud, "HAIL THE SUN", "WAKE", "Loud", 1);
        tag(&quiet, "hail the sun", "wake", "Quiet", 2);
        tag(&alone, "RADIOHEAD", "OK COMPUTER", "Airbag", 1);

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 2);
        assert_eq!(catalog.artists[0].name, "Hail the Sun");
        assert_eq!(catalog.artists[0].albums[0].name, "Wake");
        assert_eq!(catalog.artists[1].name, "RADIOHEAD");
        assert_eq!(catalog.artists[1].albums[0].name, "OK COMPUTER");
        let hail = &catalog.artists[0];
        let radio = &catalog.artists[1];
        assert_eq!(
            read_tags(&hail.albums[0].songs[0].path).artist.as_deref(),
            Some("Hail the Sun")
        );
        assert_eq!(
            read_tags(&hail.albums[0].songs[1].path).album.as_deref(),
            Some("Wake")
        );
        assert_eq!(
            read_tags(&radio.albums[0].songs[0].path).artist.as_deref(),
            Some("RADIOHEAD")
        );
        assert_eq!(
            read_tags(&radio.albums[0].songs[0].path).album.as_deref(),
            Some("OK COMPUTER")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_file_takes_the_artist_and_album_spelling_already_on_disk() {
        let root = scratch("adopt");
        let kept = root.join("kept.wav");
        write_tone(&kept);
        tag(&kept, "Hail the Sun", "Wake", "Kept", 1);
        let incoming = root.join("incoming.wav");
        write_tone(&incoming);
        tag(&incoming, "Hail The Sun", "wake", "New Song", 2);

        let filed = shelf(&root, &incoming).unwrap();
        assert_eq!(
            filed,
            root.join("Hail the Sun")
                .join("Wake")
                .join("02 New Song.wav")
        );
        assert_eq!(read_tags(&incoming).artist.as_deref(), Some("Hail the Sun"));
        assert_eq!(read_tags(&incoming).album.as_deref(), Some("Wake"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_feature_credit_files_under_the_primary_artist_and_album() {
        let root = scratch("feature");
        let anthony = root.join("anthony.wav");
        let ella = root.join("ella.wav");
        let plain = root.join("plain.wav");
        write_tone(&anthony);
        write_tone(&ella);
        write_tone(&plain);
        tag(
            &anthony,
            "Anthony Green feat. Prentiss",
            "Avalon (feat. Someone)",
            "Baby Girl feat. Prentiss",
            1,
        );
        tag(
            &ella,
            "Ella Langley ft. Riley Green",
            "Hungover",
            "You Look Like You Love Me",
            1,
        );
        tag(
            &plain,
            "Simon & Garfunkel",
            "Bridge Over Troubled Water",
            "The Boxer",
            1,
        );

        let anthony_path = shelf(&root, &anthony).unwrap();
        let ella_path = shelf(&root, &ella).unwrap();
        let plain_path = shelf(&root, &plain).unwrap();
        assert_eq!(
            anthony_path,
            root.join("Anthony Green")
                .join("Avalon")
                .join("01 Baby Girl feat. Prentiss.wav")
        );
        assert_eq!(
            ella_path,
            root.join("Ella Langley")
                .join("Hungover")
                .join("01 You Look Like You Love Me.wav")
        );
        assert_eq!(
            plain_path,
            root.join("Simon & Garfunkel")
                .join("Bridge Over Troubled Water")
                .join("01 The Boxer.wav")
        );
        assert_eq!(read_tags(&anthony).artist.as_deref(), Some("Anthony Green"));
        assert_eq!(
            read_tags(&anthony).album_artist.as_deref(),
            Some("Anthony Green")
        );
        assert_eq!(read_tags(&anthony).album.as_deref(), Some("Avalon"));
        assert_eq!(
            read_tags(&anthony).title.as_deref(),
            Some("Baby Girl feat. Prentiss")
        );
        assert_eq!(read_tags(&ella).artist.as_deref(), Some("Ella Langley"));
        assert_eq!(
            read_tags(&plain).artist.as_deref(),
            Some("Simon & Garfunkel")
        );

        fs::create_dir_all(anthony_path.parent().unwrap()).unwrap();
        fs::create_dir_all(ella_path.parent().unwrap()).unwrap();
        fs::rename(&anthony, &anthony_path).unwrap();
        fs::rename(&ella, &ella_path).unwrap();
        fs::remove_file(&plain).unwrap();
        let guest = anthony_path.parent().unwrap().join("02 Other.wav");
        write_tone(&guest);
        tag(
            &guest,
            "Anthony Green featuring Prentiss",
            "Avalon feat. Prentiss",
            "Other",
            2,
        );
        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 2);
        assert_eq!(catalog.artists[0].name, "Anthony Green");
        assert_eq!(catalog.artists[0].albums.len(), 1);
        assert_eq!(catalog.artists[0].albums[0].name, "Avalon");
        assert_eq!(catalog.artists[0].albums[0].songs.len(), 2);
        assert_eq!(catalog.artists[1].name, "Ella Langley");
        assert_eq!(read_tags(&guest).artist.as_deref(), Some("Anthony Green"));
        assert_eq!(read_tags(&guest).album.as_deref(), Some("Avalon"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_tight_feature_credit_joins_the_artist_already_on_the_album() {
        let root = scratch("tight-feature");
        let plain = root.join("plain.wav");
        let guest = root.join("guest.wav");
        let other = root.join("other.wav");
        write_tone(&plain);
        write_tone(&guest);
        write_tone(&other);
        tag(&plain, "Ella Langley", "Hungover", "You", 1);
        tag(
            &guest,
            "ELla Langley feat.Miranda Lambert",
            "Hungover",
            "Choosin Texas",
            2,
        );
        tag(&other, "Miranda Lambert", "Wildcard", "Bluebird", 1);
        let duo = root.join("duo.wav");
        let solo = root.join("solo.wav");
        write_tone(&duo);
        write_tone(&solo);
        tag(&duo, "Simon & Garfunkel", "Bridge", "The Boxer", 1);
        tag(&solo, "Simon", "Bridge", "Homeward", 2);

        let plain_path = shelf(&root, &plain).unwrap();
        fs::create_dir_all(plain_path.parent().unwrap()).unwrap();
        fs::rename(&plain, &plain_path).unwrap();
        let guest_path = shelf(&root, &guest).unwrap();
        assert_eq!(
            guest_path,
            root.join("Ella Langley")
                .join("Hungover")
                .join("02 Choosin Texas.wav")
        );
        fs::create_dir_all(guest_path.parent().unwrap()).unwrap();
        fs::rename(&guest, &guest_path).unwrap();
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        let catalog = Catalog::scan(&root);
        let names: Vec<&str> = catalog
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["Ella Langley", "Miranda Lambert", "Simon & Garfunkel"]
        );
        let bridge = catalog
            .artists
            .iter()
            .find(|artist| artist.name == "Simon & Garfunkel")
            .unwrap();
        assert_eq!(bridge.albums[0].songs.len(), 2);
        assert_eq!(catalog.artists[0].albums[0].songs.len(), 2);
        assert_eq!(
            read_tags(&guest_path).artist.as_deref(),
            Some("Ella Langley")
        );
        assert_eq!(
            read_tags(&guest_path).title.as_deref(),
            Some("Choosin Texas")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discard_removes_the_file_and_leaves_the_rest_of_the_artist() {
        let root = scratch("discard");
        let song = root
            .join("Hail the Sun")
            .join("Wake")
            .join("01 Falling.wav");
        let kept = root
            .join("Hail the Sun")
            .join("Culture Scars")
            .join("01 Stay.wav");
        fs::create_dir_all(song.parent().unwrap()).unwrap();
        fs::create_dir_all(kept.parent().unwrap()).unwrap();
        fs::write(&song, b"a").unwrap();
        fs::write(&kept, b"b").unwrap();
        let outside = scratch("outside");
        let foreign = outside.join("nope.wav");
        fs::write(&foreign, b"c").unwrap();

        discard(&root, &[song.clone(), foreign.clone(), root.clone()]);

        assert!(!song.exists());
        assert!(!song.parent().unwrap().exists());
        assert!(kept.is_file());
        assert!(root.join("Hail the Sun").is_dir());
        assert!(foreign.is_file());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn sweep_removes_leftover_images_and_empty_folders() {
        let root = scratch("sweep");
        let song = root
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        let cover = root.join("Green Day").join("Dookie").join("folder.jpg");
        let old_art = root.join("Dookie").join("scans").join("front.png");
        let old_note = root.join("Dookie").join("notes.txt");
        let loose = root.join("cover.jpg");
        let incoming = root.join("incoming").join("bob__folder.jpg");
        fs::create_dir_all(song.parent().unwrap()).unwrap();
        fs::create_dir_all(old_art.parent().unwrap()).unwrap();
        fs::create_dir_all(incoming.parent().unwrap()).unwrap();
        fs::write(&song, b"a").unwrap();
        fs::write(&cover, b"b").unwrap();
        fs::write(&old_art, b"c").unwrap();
        fs::write(&old_note, b"d").unwrap();
        fs::write(&loose, b"e").unwrap();
        fs::write(&incoming, b"f").unwrap();

        let removed = sweep(&root, Some(incoming.parent().unwrap()));

        assert!(song.is_file());
        assert!(!cover.exists());
        assert!(!old_art.exists());
        assert!(!old_note.exists());
        assert!(!root.join("Dookie").exists());
        assert!(!loose.exists());
        assert!(incoming.is_file());
        assert!(root.is_dir());
        assert!(removed.contains(&cover));
        assert!(removed.contains(&old_art));
        assert!(removed.contains(&old_note));
        assert!(removed.contains(&loose));
        assert!(!removed.contains(&song));
        assert!(!removed.contains(&incoming));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_cached_library_reads_tags_only_for_files_that_changed() {
        let root = scratch("cache");
        let kept = root.join("kept.wav");
        write_tone(&kept);
        tag(&kept, "Hail the Sun", "Wake", "Falling on Deaf Ears", 1);

        TAG_READS.with(|reads| reads.set(Some(0)));
        let catalog = Catalog::scan(&root);
        let first = TAG_READS.with(|reads| reads.get().unwrap());
        assert!(first >= 2, "{first}");

        let kept_path = catalog.artists[0].albums[0].songs[0].path.clone();
        TAG_READS.with(|reads| reads.set(Some(0)));
        let again = catalog.reconcile(&root).0;
        assert_eq!(TAG_READS.with(|reads| reads.get().unwrap()), 0);
        assert_eq!(again.artists, catalog.artists);

        let added = root.join("added.wav");
        write_tone(&added);
        tag(
            &added,
            "Hail The Sun",
            "Wake",
            "Rolling Out the Welcome Mat",
            2,
        );
        TAG_READS.with(|reads| reads.set(Some(0)));
        let grown = again.reconcile(&root).0;
        assert_eq!(TAG_READS.with(|reads| reads.get().unwrap()), 2);
        assert_eq!(grown.artists.len(), 1);
        assert_eq!(grown.artists[0].name, "Hail the Sun");
        assert_eq!(grown.artists[0].albums[0].songs.len(), 2);

        let added_path = grown.artists[0].albums[0]
            .songs
            .iter()
            .find(|song| song.title == "Rolling Out the Welcome Mat")
            .unwrap()
            .path
            .clone();
        fs::remove_file(&added_path).unwrap();
        TAG_READS.with(|reads| reads.set(Some(0)));
        let shrunk = grown.reconcile(&root).0;
        assert_eq!(TAG_READS.with(|reads| reads.get().unwrap()), 0);
        assert_eq!(shrunk.artists[0].albums[0].songs.len(), 1);
        assert_eq!(
            shrunk.artists[0].albums[0].songs[0].title,
            "Falling on Deaf Ears"
        );

        let store = root.join("library.toml");
        shrunk.store(&store, &root);
        let loaded = Catalog::load(&store, &root).unwrap();
        TAG_READS.with(|reads| reads.set(Some(0)));
        let restored = loaded.reconcile(&root).0;
        assert_eq!(TAG_READS.with(|reads| reads.get().unwrap()), 0);
        assert_eq!(restored.artists, shrunk.artists);
        assert!(Catalog::load(&store, &root.join("elsewhere")).is_none());

        tag(&kept_path, "Hail the Sun", "Wake", "Rewritten", 1);
        TAG_READS.with(|reads| reads.set(Some(0)));
        let rewritten = shrunk.reconcile(&root).0;
        assert_eq!(TAG_READS.with(|reads| reads.get().unwrap()), 2);
        assert_eq!(rewritten.artists[0].albums[0].songs[0].title, "Rewritten");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_saved_verdict_survives_a_reload_and_an_unchanged_file() {
        let root = scratch("verdict");
        let song = root.join("song.wav");
        write_tone(&song);
        tag(&song, "Ada", "Album", "Song", 1);
        let mut catalog = Catalog::scan(&root);
        let path = catalog.artists[0].albums[0].songs[0].path.clone();
        let quality = crate::quality::Quality {
            bit_depth: 16,
            sample_rate: 44_100,
            lossy_percent: Some(9),
            cutoff_hz: 20_000,
            duration_secs: Some(1),
            verdict: crate::quality::Verdict::Lossless,
            upgrade_attempted: false,
        };
        assert!(catalog.set_quality(&path, quality.clone()));
        let store = root.join("library.toml");
        catalog.store(&store, &root);
        let loaded = Catalog::load(&store, &root).unwrap();
        assert_eq!(loaded.quality(&path), Some(&quality));
        let again = loaded.reconcile(&root).0;
        assert_eq!(again.quality(&path), Some(&quality));

        let bare = format!(
            "root = \"{}\"\n\n[[songs]]\nartist = \"Ada\"\nalbum = \"Album\"\ntitle = \"Song\"\ntrack = 1\npath = \"{}\"\nlen = {}\n",
            root.display(),
            path.display(),
            fs::metadata(&path).unwrap().len(),
        );
        fs::write(&store, bare).unwrap();
        let old = Catalog::load(&store, &root).unwrap();
        assert!(old.quality(&path).is_none());
        assert_eq!(old.artists[0].albums[0].songs[0].title, "Song");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn forgetting_a_verdict_queues_the_song_again() {
        let root = scratch("forget-quality");
        let song = root.join("song.wav");
        write_tone(&song);
        tag(&song, "Ada", "Album", "Song", 1);
        let mut catalog = Catalog::scan(&root);
        let path = catalog.artists[0].albums[0].songs[0].path.clone();
        catalog.set_quality(
            &path,
            crate::quality::Quality {
                bit_depth: 16,
                sample_rate: 44_100,
                lossy_percent: Some(40),
                cutoff_hz: 12_000,
                duration_secs: Some(1),
                verdict: crate::quality::Verdict::Lossy,
                upgrade_attempted: false,
            },
        );
        assert!(catalog.mark_upgrade_attempted(&path));
        assert!(catalog.pending_analysis(Path::new("")).is_empty());
        catalog.forget_quality();
        assert!(catalog.quality(&path).is_none());
        assert_eq!(catalog.pending_analysis(Path::new("playing")), vec![path]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn the_incomplete_folder_is_left_out_of_the_catalog() {
        let root = scratch("skip-incomplete");
        let nested = root.join("incomplete");
        fs::create_dir_all(&nested).unwrap();
        let staged = nested.join("staged.wav");
        write_tone(&staged);
        tag(&staged, "Ada", "Album", "Staged", 1);
        let kept = root.join("kept.wav");
        write_tone(&kept);
        tag(&kept, "Ada", "Album", "Kept", 1);
        let catalog = Catalog::default()
            .reconcile_skipping(&root, Some(&nested))
            .0;
        let titles: Vec<&str> = catalog
            .artists
            .iter()
            .flat_map(|artist| artist.albums.iter())
            .flat_map(|album| album.songs.iter())
            .map(|song| song.title.as_str())
            .collect();
        assert!(titles.contains(&"Kept"), "{titles:?}");
        assert!(!titles.contains(&"Staged"), "{titles:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn guest_joiners_leave_the_primary_credit() {
        let cases = [
            ("Drake feat. X", "Drake"),
            ("Drake featuring X", "Drake"),
            ("Drake ft. X", "Drake"),
            ("Drake ft X", "Drake"),
            ("Drake with X", "Drake"),
            ("Drake f/ X", "Drake"),
            ("Drake f. X", "Drake"),
            ("Drake f X", "Drake"),
            ("Drake w/ X", "Drake"),
            ("Drake w. X", "Drake"),
            ("Drake w X", "Drake"),
            ("Drake vs. X", "Drake"),
            ("Drake versus X", "Drake"),
            ("ELla Langley feat.Miranda Lambert", "ELla Langley"),
            ("Simon & Garfunkel", "Simon & Garfunkel"),
            ("Wolf Alice", "Wolf Alice"),
            ("Feather", "Feather"),
            ("Avalon (feat. Someone)", "Avalon"),
            ("Avalon (with Someone)", "Avalon"),
        ];
        for (raw, want) in cases {
            assert_eq!(primary_credit(raw), want, "{raw}");
        }
    }

    #[test]
    fn a_shorter_credit_adopts_the_collaborative_album_artist() {
        let votes = vec!["Simon".to_owned(), "Simon & Garfunkel".to_owned()];
        assert_eq!(
            unify_credits(&votes),
            vec![
                "Simon & Garfunkel".to_owned(),
                "Simon & Garfunkel".to_owned()
            ]
        );
    }

    #[test]
    fn unrelated_artists_on_one_album_stay_separate() {
        let votes = vec!["Green Day".to_owned(), "Radiohead".to_owned()];
        assert_eq!(unify_credits(&votes), votes);
    }

    #[test]
    fn guest_wording_files_under_the_album_artist_without_a_sibling() {
        let root = scratch("guests");
        let cases = [
            ("with", "Drake with Party"),
            ("slash-f", "Drake f/ Future"),
            ("bare-f", "Drake f Future"),
            ("slash-w", "Drake w/ Party"),
            ("bare-w", "Drake w Party"),
        ];
        for (name, artist) in cases {
            let source = root.join(format!("{name}.wav"));
            write_tone(&source);
            tag(&source, artist, "Views", name, 1);
            let filed = shelf(&root, &source).unwrap();
            assert_eq!(
                filed,
                root.join("Drake")
                    .join("Views")
                    .join(format!("01 {name}.wav")),
                "{artist}"
            );
            assert_eq!(read_tags(&source).artist.as_deref(), Some("Drake"));
            assert_eq!(read_tags(&source).album_artist.as_deref(), Some("Drake"));
            fs::remove_file(&source).unwrap();
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_album_artist_tag_overrides_the_track_artist() {
        let root = scratch("album-artist");
        let source = root.join("track.wav");
        write_tone(&source);
        tag_album_artist(
            &source,
            "Someone feat. Drake",
            "Drake feat. Future",
            "Scorpion",
            "Song",
            1,
        );
        let filed = shelf(&root, &source).unwrap();
        assert_eq!(
            filed,
            root.join("Drake").join("Scorpion").join("01 Song.wav")
        );
        assert_eq!(read_tags(&source).artist.as_deref(), Some("Drake"));
        assert_eq!(read_tags(&source).album_artist.as_deref(), Some("Drake"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_compilation_keeps_each_artist() {
        let root = scratch("compilation");
        let green = root.join("green.wav");
        let radio = root.join("radio.wav");
        write_tone(&green);
        write_tone(&radio);
        tag(&green, "Green Day", "Now", "Basket", 1);
        tag(&radio, "Radiohead", "Now", "Airbag", 2);
        let catalog = Catalog::scan(&root);
        let names: Vec<&str> = catalog
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect();
        assert_eq!(names, vec!["Green Day", "Radiohead"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_guest_folder_is_renamed_to_the_album_artist() {
        let root = scratch("relocate");
        let guest = root
            .join("Ella Langley with Riley Green")
            .join("Hungover")
            .join("01 You.wav");
        let other = root.join("loose").join("02 Choosin.wav");
        fs::create_dir_all(guest.parent().unwrap()).unwrap();
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        write_tone(&guest);
        write_tone(&other);
        tag(
            &guest,
            "Ella Langley with Riley Green",
            "Hungover",
            "You",
            1,
        );
        tag(
            &other,
            "Ella Langley f/ Miranda Lambert",
            "Hungover",
            "Choosin",
            2,
        );

        let (catalog, moved) = Catalog::default().reconcile(&root);
        assert_eq!(moved.len(), 2);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Ella Langley");
        assert_eq!(catalog.artists[0].albums[0].name, "Hungover");
        assert_eq!(catalog.artists[0].albums[0].songs.len(), 2);
        let folder = root.join("Ella Langley").join("Hungover");
        assert!(folder.join("01 You.wav").is_file());
        assert!(folder.join("02 Choosin.wav").is_file());
        assert!(!root.join("Ella Langley with Riley Green").exists());
        assert!(!root.join("loose").exists());
        assert_eq!(
            read_tags(&folder.join("01 You.wav"))
                .album_artist
                .as_deref(),
            Some("Ella Langley")
        );
        sweep(&root, None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_folder_preview_lists_the_primary_artist_and_leaves_the_file() {
        let root = scratch("preview-folders");
        let guest = root
            .join("Eminem f Thyme")
            .join("Infinite (feat. Someone)")
            .join("07 Open Mic.flac");
        fs::create_dir_all(guest.parent().unwrap()).unwrap();
        fs::write(&guest, b"not audio tags").unwrap();

        let catalog = Catalog::preview_skipping(&root, None);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Eminem");
        assert_eq!(catalog.artists[0].albums[0].name, "Infinite");
        assert_eq!(catalog.artists[0].albums[0].songs[0].path, guest);
        assert!(guest.is_file());
        assert!(root.join("Eminem f Thyme").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_lowercase_significant_word_is_capitalized() {
        let root = scratch("inch");
        let path = root
            .join("Nine inch Nails")
            .join("Broken")
            .join("01 Wish.wav");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_tone(&path);
        tag(&path, "Nine inch Nails", "Broken", "Wish", 1);

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Nine Inch Nails");
        let filed = root
            .join("Nine Inch Nails")
            .join("Broken")
            .join("01 Wish.wav");
        assert!(filed.is_file());
        assert!(!root.join("Nine inch Nails").exists());
        assert_eq!(read_tags(&filed).artist.as_deref(), Some("Nine Inch Nails"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_featured_artist_rolls_under_the_artist_on_another_album() {
        let root = scratch("featured-nin");
        let kept = root
            .join("Nine Inch Nails")
            .join("The Fragile")
            .join("01 Somewhat Damaged.wav");
        let ampersand = root
            .join("Nine Inch Nails & David Bowie")
            .join("Live")
            .join("01 Hallo Spaceboy.wav");
        let word = root
            .join("Nine inch Nails and David Bowie")
            .join("St Louis")
            .join("01 A Small Plot Of Land.wav");
        for path in [&kept, &ampersand, &word] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_tone(path);
        }
        tag(
            &kept,
            "Nine Inch Nails",
            "The Fragile",
            "Somewhat Damaged",
            1,
        );
        tag(
            &ampersand,
            "Nine Inch Nails & David Bowie",
            "Live",
            "Hallo Spaceboy",
            1,
        );
        tag(
            &word,
            "Nine inch Nails and David Bowie",
            "St Louis",
            "A Small Plot Of Land",
            1,
        );

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Nine Inch Nails");
        assert_eq!(catalog.artists[0].albums.len(), 3);
        let home = root.join("Nine Inch Nails");
        assert!(
            home.join("The Fragile")
                .join("01 Somewhat Damaged.wav")
                .is_file()
        );
        assert!(home.join("Live").join("01 Hallo Spaceboy.wav").is_file());
        assert!(
            home.join("St Louis")
                .join("01 A Small Plot Of Land.wav")
                .is_file()
        );
        assert!(!root.join("Nine Inch Nails & David Bowie").exists());
        assert!(!root.join("Nine inch Nails and David Bowie").exists());
        assert_eq!(
            read_tags(&home.join("Live").join("01 Hallo Spaceboy.wav"))
                .artist
                .as_deref(),
            Some("Nine Inch Nails")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lookalike_punctuation_is_one_artist() {
        let root = scratch("punct");
        let straight = root
            .join("Jack's Mannequin")
            .join("Transit")
            .join("01 Dark Blue.wav");
        let curly = root
            .join("Jack\u{2019}s Mannequin")
            .join("Passenger")
            .join("01 Caves.wav");
        for path in [&straight, &curly] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_tone(path);
        }
        tag(&straight, "Jack's Mannequin", "Transit", "Dark Blue", 1);
        tag(&curly, "Jack\u{2019}s Mannequin", "Passenger", "Caves", 1);

        let catalog = Catalog::scan(&root);
        assert_eq!(catalog.artists.len(), 1);
        assert_eq!(catalog.artists[0].name, "Jack's Mannequin");
        assert_eq!(catalog.artists[0].albums.len(), 2);
        assert!(!root.join("Jack\u{2019}s Mannequin").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_duo_without_another_album_stays_one_artist() {
        let root = scratch("duo");
        let duo = root
            .join("Simon & Garfunkel")
            .join("Bridge")
            .join("01 The Boxer.wav");
        let shorter = root.join("loose").join("02 Cecilia.wav");
        let band = root
            .join("Coheed and Cambria")
            .join("Second Stage")
            .join("01 In Keeping.wav");
        for path in [&duo, &shorter, &band] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            write_tone(path);
        }
        tag(&duo, "Simon & Garfunkel", "Bridge", "The Boxer", 1);
        tag(&shorter, "Simon", "Bridge", "Cecilia", 2);
        tag(&band, "Coheed and Cambria", "Second Stage", "In Keeping", 1);

        let catalog = Catalog::scan(&root);
        let names: Vec<&str> = catalog
            .artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect();
        assert_eq!(names, vec!["Coheed and Cambria", "Simon & Garfunkel"]);
        assert_eq!(catalog.artists[1].albums[0].songs.len(), 2);
        assert!(
            root.join("Simon & Garfunkel")
                .join("Bridge")
                .join("02 Cecilia.wav")
                .is_file()
        );
        assert!(!root.join("Simon").exists());
        assert!(root.join("Coheed and Cambria").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    fn tag(path: &Path, artist: &str, album: &str, title: &str, track: u32) {
        let mut file = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist(artist.to_owned());
        tag.set_album(album.to_owned());
        tag.set_title(title.to_owned());
        tag.set_track(track);
        file.insert_tag(tag);
        file.save_to_path(path, WriteOptions::default()).unwrap();
    }

    fn tag_album_artist(
        path: &Path,
        artist: &str,
        album_artist: &str,
        album: &str,
        title: &str,
        track: u32,
    ) {
        let mut file = lofty::read_from_path(path).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_artist(artist.to_owned());
        tag.insert_text(ItemKey::AlbumArtist, album_artist.to_owned());
        tag.set_album(album.to_owned());
        tag.set_title(title.to_owned());
        tag.set_track(track);
        file.insert_tag(tag);
        file.save_to_path(path, WriteOptions::default()).unwrap();
    }

    fn write_tone(path: &Path) {
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
        fs::write(path, bytes).unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
