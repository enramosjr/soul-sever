//! Local share index.
//!
//! [`rescan`] walks the configured folders inside `spawn_blocking`. The session
//! sends the public counts and answers a browse from [`ShareIndex::list_for`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use lofty::file::AudioFile;
use lofty::properties::FileProperties;

use crate::config::{Buddy, Shares};
use crate::protocol::peer::{
    FILE_ATTRIBUTE_BIT_DEPTH, FILE_ATTRIBUTE_BITRATE, FILE_ATTRIBUTE_DURATION,
    FILE_ATTRIBUTE_SAMPLE_RATE, FILE_ATTRIBUTE_VBR, SharedFile, SharedFileListResponse,
    SharedFolder,
};

/// Files and distinct directories at one share level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelCounts {
    pub files: u32,
    pub folders: u32,
}

fn tally(files: &[IndexedFile], level: ShareLevel) -> LevelCounts {
    let mut folders = HashSet::new();
    let mut count = 0u32;
    for file in files.iter().filter(|file| file.level == level) {
        count = count.saturating_add(1);
        folders.insert(file.directory.as_str());
    }
    LevelCounts {
        files: count,
        folders: u32::try_from(folders.len()).unwrap_or(u32::MAX),
    }
}

/// Who is asking for a share list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Public,
    Buddy,
    Trusted,
}

/// Which configured folder a file came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareLevel {
    Public,
    Buddy,
    Trusted,
}

impl ShareLevel {
    fn visible_to(self, access: Access) -> bool {
        match self {
            Self::Public => true,
            Self::Buddy => matches!(access, Access::Buddy | Access::Trusted),
            Self::Trusted => matches!(access, Access::Trusted),
        }
    }
}

/// One indexed file. `id` is the position in [`ShareIndex::files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub id: u32,
    pub path: PathBuf,
    pub name: String,
    pub directory: String,
    pub size: u64,
    pub level: ShareLevel,
    pub attributes: Vec<(u32, u32)>,
}

/// Filenames and the tokens inside them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareIndex {
    files: Vec<IndexedFile>,
    words: HashMap<String, Vec<u32>>,
}

impl ShareIndex {
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            words: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// File and folder totals for public, buddy, and trusted, in that order.
    pub fn counts(&self) -> [LevelCounts; 3] {
        [
            tally(&self.files, ShareLevel::Public),
            tally(&self.files, ShareLevel::Buddy),
            tally(&self.files, ShareLevel::Trusted),
        ]
    }

    pub fn files(&self) -> &[IndexedFile] {
        &self.files
    }

    /// Files whose filename contains `token`. The token is one word, compared
    /// after lowercasing. A query with more than one word matches nothing.
    pub fn lookup(&self, token: &str) -> Vec<&IndexedFile> {
        let mut parts = tokens(token);
        let Some(key) = parts.next() else {
            return Vec::new();
        };
        if parts.next().is_some() {
            return Vec::new();
        }
        let Some(ids) = self.words.get(&key) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| self.files.get(*id as usize))
            .collect()
    }

    /// Drops `from` and any previous copy of `to`, then indexes the file now at `to`.
    ///
    /// The virtual directory is the share root's folder name plus the relative
    /// path, the same shape [`walk`] builds. A file under no configured root is
    /// shared publicly from `download_root`, and that root is appended to
    /// `shares.public`. The returned path is that newly added root.
    pub fn store_moved(
        &mut self,
        shares: &mut Shares,
        download_root: &Path,
        from: &Path,
        to: &Path,
    ) -> Option<PathBuf> {
        self.files
            .retain(|file| file.path != from && file.path != to);
        let Some(mut prepared) = prepare_moved(shares, download_root, to) else {
            self.finish_ids();
            return None;
        };
        let Ok(id) = u32::try_from(self.files.len()) else {
            self.finish_ids();
            return None;
        };
        if let Some(root) = prepared.added.clone() {
            shares.public.push(root);
        }
        prepared.file.id = id;
        self.files.push(prepared.file);
        self.finish_ids();
        prepared.added
    }

    /// Drops indexed files whose disk path is in `paths`.
    pub fn forget(&mut self, paths: &[PathBuf]) {
        let before = self.files.len();
        self.files
            .retain(|file| !paths.iter().any(|path| path == &file.path));
        if self.files.len() != before {
            self.finish_ids();
        }
    }

    fn finish_ids(&mut self) {
        for (index, file) in self.files.iter_mut().enumerate() {
            file.id = u32::try_from(index).unwrap_or(u32::MAX);
        }
        self.words = index_words(&self.files);
    }

    /// Visible folders for `username`, plus locked folders that user cannot browse.
    /// A trusted buddy also sees buddy-level files. Anyone sees public files.
    pub fn list_for(&self, username: &str, buddies: &[Buddy]) -> SharedFileListResponse {
        let access = access_for(username, buddies);
        let mut visible: HashMap<String, Vec<SharedFile>> = HashMap::new();
        let mut private: HashMap<String, Vec<SharedFile>> = HashMap::new();
        for file in &self.files {
            let folders = if file.level.visible_to(access) {
                &mut visible
            } else {
                &mut private
            };
            folders
                .entry(file.directory.clone())
                .or_default()
                .push(SharedFile {
                    name: file.name.clone(),
                    size: file.size,
                    attributes: file.attributes.clone(),
                });
        }
        SharedFileListResponse {
            list: into_folders(visible),
            private_list: into_folders(private),
        }
    }
}

fn access_for(username: &str, buddies: &[Buddy]) -> Access {
    match buddies.iter().find(|buddy| buddy.name == username) {
        Some(buddy) if buddy.trusted => Access::Trusted,
        Some(_) => Access::Buddy,
        None => Access::Public,
    }
}

fn into_folders(groups: HashMap<String, Vec<SharedFile>>) -> Vec<SharedFolder> {
    let mut folders: Vec<SharedFolder> = groups
        .into_iter()
        .map(|(directory, mut files)| {
            files.sort_by(|left, right| left.name.cmp(&right.name));
            SharedFolder { directory, files }
        })
        .collect();
    folders.sort_by(|left, right| left.directory.cmp(&right.directory));
    folders
}

/// Walk `shares` off the async worker. An unreadable configured folder fails the scan.
pub async fn rescan(shares: &Shares) -> Result<ShareIndex, ShareError> {
    let shares = shares.clone();
    match tokio::task::spawn_blocking(move || scan(&shares)).await {
        Ok(result) => result,
        Err(_) => Err(ShareError::ScanFailed),
    }
}

fn scan(shares: &Shares) -> Result<ShareIndex, ShareError> {
    let mut files = Vec::new();
    for (root, level) in roots(shares) {
        let virtual_root = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| ShareError::NotDirectory {
                path: root.to_path_buf(),
            })?;
        walk(root, virtual_root, level, &shares.exclude, &mut files)?;
    }
    let words = index_words(&files);
    Ok(ShareIndex { files, words })
}

struct PreparedMove {
    file: IndexedFile,
    added: Option<PathBuf>,
}

fn prepare_moved(shares: &Shares, download_root: &Path, to: &Path) -> Option<PreparedMove> {
    let meta = fs::symlink_metadata(to).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let name = to.file_name()?.to_str()?.to_owned();
    if excluded(&name, &shares.exclude) {
        return None;
    }
    let (root, level, added) = match covering(shares, to) {
        Some((root, level)) => (root, level, None),
        None if !download_root.as_os_str().is_empty() && to.starts_with(download_root) => (
            download_root.to_path_buf(),
            ShareLevel::Public,
            Some(download_root.to_path_buf()),
        ),
        None => return None,
    };
    Some(PreparedMove {
        file: IndexedFile {
            id: 0,
            path: to.to_path_buf(),
            name,
            directory: virtual_directory(&root, to)?,
            size: meta.len(),
            level,
            attributes: audio_attributes(to),
        },
        added,
    })
}

fn covering(shares: &Shares, file: &Path) -> Option<(PathBuf, ShareLevel)> {
    roots(shares)
        .into_iter()
        .filter(|(root, _)| file.starts_with(root))
        .max_by_key(|(root, _)| root.components().count())
        .map(|(root, level)| (root.to_path_buf(), level))
}

fn virtual_directory(root: &Path, file: &Path) -> Option<String> {
    let virtual_root = root.file_name()?.to_str()?;
    let relative = file.parent()?.strip_prefix(root).ok()?;
    let mut directory = virtual_root.to_owned();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        directory.push('\\');
        directory.push_str(part.to_str()?);
    }
    Some(directory)
}

fn roots(shares: &Shares) -> Vec<(&Path, ShareLevel)> {
    let mut roots = Vec::new();
    roots.extend(
        shares
            .public
            .iter()
            .map(|path| (path.as_path(), ShareLevel::Public)),
    );
    roots.extend(
        shares
            .buddy
            .iter()
            .map(|path| (path.as_path(), ShareLevel::Buddy)),
    );
    roots.extend(
        shares
            .trusted
            .iter()
            .map(|path| (path.as_path(), ShareLevel::Trusted)),
    );
    roots
}

fn walk(
    dir: &Path,
    virtual_dir: &str,
    level: ShareLevel,
    exclude: &[String],
    files: &mut Vec<IndexedFile>,
) -> Result<(), ShareError> {
    let metadata = fs::metadata(dir).map_err(|source| ShareError::Unreadable {
        path: dir.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(ShareError::NotDirectory {
            path: dir.to_path_buf(),
        });
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| ShareError::Unreadable {
        path: dir.to_path_buf(),
        source,
    })? {
        entries.push(entry.map_err(|source| ShareError::Unreadable {
            path: dir.to_path_buf(),
            source,
        })?);
    }
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type().map_err(|source| ShareError::Unreadable {
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if excluded(&name, exclude) {
            continue;
        }
        if file_type.is_dir() {
            let child = format!("{virtual_dir}\\{name}");
            walk(&entry.path(), &child, level, exclude, files)?;
        } else if file_type.is_file() {
            let path = entry.path();
            let size = entry
                .metadata()
                .map_err(|source| ShareError::Unreadable {
                    path: path.clone(),
                    source,
                })?
                .len();
            let id = u32::try_from(files.len()).map_err(|_| ShareError::TooManyFiles)?;
            files.push(IndexedFile {
                id,
                path: path.clone(),
                name,
                directory: virtual_dir.to_owned(),
                size,
                level,
                attributes: audio_attributes(&path),
            });
        }
    }
    Ok(())
}

fn excluded(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| glob_match(pattern, name))
}

fn glob_match(pattern: &str, name: &str) -> bool {
    glob_bytes(pattern.as_bytes(), name.as_bytes())
}

fn glob_bytes(pattern: &[u8], name: &[u8]) -> bool {
    let mut pattern_index = 0;
    let mut name_index = 0;
    let mut star = None;
    let mut resume = 0;
    while name_index < name.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            resume = name_index;
            pattern_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == name[name_index] {
            pattern_index += 1;
            name_index += 1;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            resume += 1;
            name_index = resume;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn index_words(files: &[IndexedFile]) -> HashMap<String, Vec<u32>> {
    let mut words: HashMap<String, Vec<u32>> = HashMap::new();
    for file in files {
        let mut seen = HashSet::new();
        for token in tokens(&file.name) {
            if seen.insert(token.clone()) {
                words.entry(token).or_default().push(file.id);
            }
        }
    }
    words
}

fn tokens(name: &str) -> impl Iterator<Item = String> + '_ {
    name.split(|character: char| !character.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
}

fn audio_attributes(path: &Path) -> Vec<(u32, u32)> {
    if !is_audio(path) {
        return Vec::new();
    }
    let Ok(tagged) = lofty::read_from_path(path) else {
        return Vec::new();
    };
    attributes_from(tagged.properties(), None)
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
            | "mpc"
            | "ogg"
            | "opus"
            | "wav"
            | "wma"
            | "wv"
    )
}

/// Attribute order follows Nicotine+ `pack_file_info`. Lossless files (a bit
/// depth is present) omit bitrate and VBR. `vbr` is written only when the
/// caller knows it. lofty 0.24's [`FileProperties`] does not report VBR, so
/// [`rescan`] passes `None`.
fn attributes_from(properties: &FileProperties, vbr: Option<bool>) -> Vec<(u32, u32)> {
    let duration = u32::try_from(properties.duration().as_secs())
        .ok()
        .filter(|seconds| *seconds > 0);
    let sample_rate = properties.sample_rate().filter(|rate| *rate > 0);
    let bit_depth = properties
        .bit_depth()
        .filter(|depth| *depth > 0)
        .map(u32::from);
    let bitrate = properties.audio_bitrate().filter(|rate| *rate > 0);
    let mut attributes = Vec::new();
    if bit_depth.is_some() {
        push_attr(&mut attributes, FILE_ATTRIBUTE_DURATION, duration);
        push_attr(&mut attributes, FILE_ATTRIBUTE_SAMPLE_RATE, sample_rate);
        push_attr(&mut attributes, FILE_ATTRIBUTE_BIT_DEPTH, bit_depth);
    } else {
        push_attr(&mut attributes, FILE_ATTRIBUTE_BITRATE, bitrate);
        push_attr(&mut attributes, FILE_ATTRIBUTE_DURATION, duration);
        if let Some(flag) = vbr {
            attributes.push((FILE_ATTRIBUTE_VBR, u32::from(flag)));
        }
    }
    attributes
}

fn push_attr(attributes: &mut Vec<(u32, u32)>, code: u32, value: Option<u32>) {
    if let Some(value) = value {
        attributes.push((code, value));
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("share folder {path} could not be read: {source}")]
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("share folder {path} is not a directory")]
    NotDirectory { path: PathBuf },
    #[error("share index exceeds 2^32 files")]
    TooManyFiles,
    #[error("share scan task ended before it finished")]
    ScanFailed,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const TONE: &[u8] = include_bytes!("../../tests/fixtures/tone.wav");

    #[test]
    fn attribute_order_follows_nicotine() {
        let lossy = FileProperties::new(
            Duration::from_secs(180),
            Some(320),
            Some(320),
            Some(44_100),
            None,
            Some(2),
            None,
        );
        assert_eq!(
            attributes_from(&lossy, Some(true)),
            vec![
                (FILE_ATTRIBUTE_BITRATE, 320),
                (FILE_ATTRIBUTE_DURATION, 180),
                (FILE_ATTRIBUTE_VBR, 1),
            ]
        );
        let lossless = FileProperties::new(
            Duration::from_secs(3),
            Some(1_411),
            Some(1_411),
            Some(44_100),
            Some(16),
            Some(2),
            None,
        );
        assert_eq!(
            attributes_from(&lossless, Some(true)),
            vec![
                (FILE_ATTRIBUTE_DURATION, 3),
                (FILE_ATTRIBUTE_SAMPLE_RATE, 44_100),
                (FILE_ATTRIBUTE_BIT_DEPTH, 16),
            ]
        );
    }

    #[tokio::test]
    async fn rescan_counts_text_and_audio_and_hides_buddy_files() {
        let root = scratch();
        let public = root.join("public");
        let buddy = root.join("buddy");
        let trusted = root.join("trusted");
        fs::create_dir_all(&public).unwrap();
        fs::create_dir_all(&buddy).unwrap();
        fs::create_dir_all(&trusted).unwrap();
        fs::write(public.join("notes.txt"), b"hello").unwrap();
        fs::write(public.join("tone.wav"), TONE).unwrap();
        fs::write(public.join("noise.skip"), b"nope").unwrap();
        fs::write(buddy.join("secret.txt"), b"hidden").unwrap();
        fs::write(trusted.join("vault.txt"), b"vault").unwrap();

        let shares = Shares {
            public: vec![public],
            buddy: vec![buddy],
            trusted: vec![trusted],
            exclude: vec!["*.skip".to_owned()],
        };
        let buddies = vec![
            Buddy {
                name: "bob".to_owned(),
                note: String::new(),
                notify: false,
                prioritized: false,
                trusted: false,
            },
            Buddy {
                name: "carol".to_owned(),
                note: String::new(),
                notify: false,
                prioritized: false,
                trusted: true,
            },
        ];
        let index = rescan(&shares).await.unwrap();
        assert_eq!(index.len(), 4);
        assert!(index.files().iter().any(|file| file.name == "notes.txt"));
        assert!(index.files().iter().any(|file| file.name == "tone.wav"));
        assert!(index.files().iter().all(|file| file.name != "noise.skip"));

        let tone = index
            .files()
            .iter()
            .find(|file| file.name == "tone.wav")
            .unwrap();
        assert!(
            tone.attributes
                .contains(&(FILE_ATTRIBUTE_SAMPLE_RATE, 8_000))
        );
        assert!(tone.attributes.contains(&(FILE_ATTRIBUTE_BIT_DEPTH, 16)));

        let notes = index.lookup("notes");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].name, "notes.txt");

        let public_list = decode_names(&index, "stranger", &buddies);
        assert!(public_list.contains(&"notes.txt".to_owned()));
        assert!(public_list.contains(&"tone.wav".to_owned()));
        assert!(!public_list.contains(&"secret.txt".to_owned()));
        assert!(!public_list.contains(&"vault.txt".to_owned()));

        let buddy_list = decode_names(&index, "bob", &buddies);
        assert!(buddy_list.contains(&"secret.txt".to_owned()));
        assert!(!buddy_list.contains(&"vault.txt".to_owned()));

        let trusted_list = decode_names(&index, "carol", &buddies);
        assert!(trusted_list.contains(&"secret.txt".to_owned()));
        assert!(trusted_list.contains(&"vault.txt".to_owned()));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_moved_file_replaces_the_indexed_path() {
        let root = scratch();
        let public = root.join("library");
        let old = public.join("01.wav");
        let filed = public
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        fs::create_dir_all(&public).unwrap();
        fs::write(&old, TONE).unwrap();
        let mut shares = Shares {
            public: vec![public.clone()],
            ..Shares::default()
        };
        let mut index = rescan(&shares).await.unwrap();
        fs::create_dir_all(filed.parent().unwrap()).unwrap();
        fs::write(&filed, TONE).unwrap();
        let added = index.store_moved(&mut shares, &public, &old, &filed);
        assert!(added.is_none());
        assert_eq!(shares.public, vec![public]);
        assert_eq!(index.len(), 1);
        let song = &index.files()[0];
        assert_eq!(song.path, filed);
        assert_eq!(song.directory, "library\\Green Day\\Dookie");
        assert_eq!(song.name, "01 Basket Case.wav");
        assert_eq!(song.level, ShareLevel::Public);
        assert_eq!(
            format!("{}\\{}", song.directory, song.name),
            "library\\Green Day\\Dookie\\01 Basket Case.wav"
        );
        assert!(index.lookup("basket").iter().any(|file| file.path == filed));
        let names = decode_names(&index, "stranger", &[]);
        assert!(names.contains(&"01 Basket Case.wav".to_owned()));
        assert!(!names.contains(&"01.wav".to_owned()));
        index.forget(std::slice::from_ref(&filed));
        assert!(index.is_empty());
        assert!(index.lookup("basket").is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_moved_file_outside_a_share_becomes_the_advertised_public_file() {
        let root = scratch();
        let public = root.join("library");
        let old = public.join("01.wav");
        let filed = public
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        fs::create_dir_all(filed.parent().unwrap()).unwrap();
        fs::write(&filed, TONE).unwrap();
        let mut shares = Shares::default();
        let mut index = ShareIndex::empty();
        let added = index
            .store_moved(&mut shares, &public, &old, &filed)
            .unwrap();
        assert_eq!(added, public);
        assert_eq!(shares.public, vec![public]);
        assert_eq!(index.counts()[0].files, 1);
        assert_eq!(index.counts()[0].folders, 1);
        let names = decode_names(&index, "stranger", &[]);
        assert_eq!(names, vec!["01 Basket Case.wav".to_owned()]);
        assert!(index.files().iter().all(|file| file.path != old));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_moved_file_keeps_its_buddy_share_level() {
        let root = scratch();
        let folder = root.join("library");
        let old = folder.join("01.wav");
        let filed = folder
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        fs::create_dir_all(&folder).unwrap();
        fs::write(&old, TONE).unwrap();
        let mut shares = Shares {
            buddy: vec![folder.clone()],
            ..Shares::default()
        };
        let mut index = rescan(&shares).await.unwrap();
        fs::create_dir_all(filed.parent().unwrap()).unwrap();
        fs::write(&filed, TONE).unwrap();
        assert!(
            index
                .store_moved(&mut shares, &folder, &old, &filed)
                .is_none()
        );
        assert!(shares.public.is_empty());
        assert_eq!(index.counts()[0].files, 0);
        assert_eq!(index.counts()[1].files, 1);
        assert_eq!(index.files()[0].level, ShareLevel::Buddy);
        let buddy = Buddy {
            name: "carol".to_owned(),
            note: String::new(),
            notify: false,
            prioritized: false,
            trusted: false,
        };
        assert!(decode_names(&index, "stranger", std::slice::from_ref(&buddy)).is_empty());
        assert_eq!(
            decode_names(&index, "carol", std::slice::from_ref(&buddy)),
            vec!["01 Basket Case.wav".to_owned()]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_excluded_moved_file_is_not_shared() {
        let root = scratch();
        let public = root.join("library");
        let old = public.join("01.wav");
        let filed = public
            .join("Green Day")
            .join("Dookie")
            .join("01 Basket Case.wav");
        fs::create_dir_all(filed.parent().unwrap()).unwrap();
        fs::write(&old, TONE).unwrap();
        fs::write(&filed, TONE).unwrap();
        let mut shares = Shares {
            public: vec![public.clone()],
            exclude: vec!["*.wav".to_owned()],
            ..Shares::default()
        };
        let mut index = ShareIndex::empty();
        index.files.push(IndexedFile {
            id: 0,
            path: old.clone(),
            name: "01.wav".to_owned(),
            directory: "library".to_owned(),
            size: 60,
            level: ShareLevel::Public,
            attributes: Vec::new(),
        });
        index.finish_ids();
        assert!(
            index
                .store_moved(&mut shares, &public, &old, &filed)
                .is_none()
        );
        assert_eq!(shares.public, vec![public]);
        assert!(index.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    fn decode_names(index: &ShareIndex, username: &str, buddies: &[Buddy]) -> Vec<String> {
        let response = index.list_for(username, buddies);
        let decoded = SharedFileListResponse::decode(&response.encode().unwrap()).unwrap();
        decoded
            .list
            .into_iter()
            .flat_map(|folder| folder.files.into_iter().map(|file| file.name))
            .collect()
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-shares-{}-{}",
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
