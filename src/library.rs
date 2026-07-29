use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use lofty::{
    file::AudioFile,
    prelude::{Accessor, TaggedFileExt},
    read_from_path,
    tag::ItemKey,
};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
    pub bytes: u64,
}

pub fn scan(root: &Path) -> (Vec<Track>, usize) {
    let mut tracks = Vec::new();
    let mut unreadable = 0;

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() || !is_supported_audio(entry.path()) {
            continue;
        }

        match read_track(entry.path()) {
            Ok(track) => tracks.push(track),
            Err(_) => unreadable += 1,
        }
    }
    tracks.sort_by(|left, right| {
        left.artist
            .cmp(&right.artist)
            .then(left.album.cmp(&right.album))
            .then(left.title.cmp(&right.title))
    });
    (tracks, unreadable)
}

fn is_supported_audio(path: &Path) -> bool {
    matches!(path.extension().and_then(|extension| extension.to_str()).map(|extension| extension.to_ascii_lowercase()),
        Some(extension) if matches!(extension.as_str(), "mp3" | "m4a" | "mp4" | "aac" | "flac" | "wav" | "ogg" | "opus"))
}

fn read_track(path: &Path) -> lofty::error::Result<Track> {
    let tagged_file = read_from_path(path)?;
    let tag = tagged_file.primary_tag();
    let fallback_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled");
    Ok(Track {
        path: path.to_path_buf(),
        title: tag
            .and_then(|tag| tag.title())
            .map(|value| value.into_owned())
            .unwrap_or_else(|| fallback_title.to_owned()),
        artist: tag
            .and_then(|tag| tag.artist())
            .map(|value| value.into_owned())
            .unwrap_or_else(|| "Unknown artist".into()),
        album_artist: tag
            .and_then(|tag| tag.get_string(ItemKey::AlbumArtist))
            .map(ToOwned::to_owned)
            .filter(|artist| !artist.trim().is_empty())
            .unwrap_or_else(|| {
                main_artist_from_credit(tag.and_then(|tag| tag.artist()).as_deref())
            }),
        album: tag
            .and_then(|tag| tag.album())
            .map(|value| value.into_owned())
            .unwrap_or_else(|| "Unknown album".into()),
        track_number: tag.and_then(|tag| tag.track()),
        duration: Some(tagged_file.properties().duration()),
        bytes: fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    })
}

pub fn main_artist_from_credit(artist: Option<&str>) -> String {
    let artist = artist.unwrap_or("Unknown artist").trim();
    let lowercase = artist.to_ascii_lowercase();
    for marker in [
        " featuring ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " & ",
        "; ",
    ] {
        if let Some(position) = lowercase.find(marker) {
            return artist[..position].trim().to_owned();
        }
    }
    artist.to_owned()
}

#[cfg(test)]
mod tests {
    use super::main_artist_from_credit;

    #[test]
    fn extracts_primary_artist_from_common_feature_credits() {
        for credit in [
            "50 Cent feat. Eminem",
            "50 Cent & G-Unit",
            "50 Cent; G-Unit",
        ] {
            assert_eq!(main_artist_from_credit(Some(credit)), "50 Cent");
        }
    }

    #[test]
    fn preserves_an_artist_without_feature_credit() {
        assert_eq!(main_artist_from_credit(Some("Nina Simone")), "Nina Simone");
    }
}
