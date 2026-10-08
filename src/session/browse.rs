//! Browse rows from a peer share list or a folder-contents reply.

use crate::model::{BrowseRow, SearchHit};
use crate::protocol::peer::{
    FILE_ATTRIBUTE_BIT_DEPTH, FILE_ATTRIBUTE_BITRATE, FILE_ATTRIBUTE_DURATION,
    FILE_ATTRIBUTE_SAMPLE_RATE, SharedFile, SharedFolder,
};

/// One row per file. Two files become two rows.
pub fn rows_from_folders(folders: &[SharedFolder]) -> Vec<BrowseRow> {
    let mut rows = Vec::new();
    for folder in folders {
        for file in &folder.files {
            rows.push(BrowseRow {
                depth: 1,
                name: file.name.clone(),
                size: Some(file.size),
                bitrate: attribute(file, FILE_ATTRIBUTE_BITRATE),
                duration: attribute(file, FILE_ATTRIBUTE_DURATION),
            });
        }
    }
    rows
}

/// Files from a folder-contents reply, with `directory\name` paths.
pub fn hits_from_folders(user: &str, folders: &[SharedFolder]) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for folder in folders {
        let directory = folder.directory.trim_end_matches(['\\', '/']);
        for file in &folder.files {
            let path = if directory.is_empty() {
                file.name.clone()
            } else {
                format!("{directory}\\{}", file.name)
            };
            hits.push(SearchHit {
                user: user.to_owned(),
                path,
                size: file.size,
                bitrate: attribute(file, FILE_ATTRIBUTE_BITRATE),
                duration: attribute(file, FILE_ATTRIBUTE_DURATION),
                bit_depth: attribute(file, FILE_ATTRIBUTE_BIT_DEPTH),
                sample_rate: attribute(file, FILE_ATTRIBUTE_SAMPLE_RATE),
                queue: 0,
                free_slot: true,
                upload_speed: 0,
                country: String::new(),
            });
        }
    }
    hits
}

fn attribute(file: &SharedFile, code: u32) -> Option<u32> {
    file.attributes
        .iter()
        .find(|(found, _)| *found == code)
        .map(|(_, value)| *value)
}
