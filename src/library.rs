use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use lofty::{
    config::WriteOptions,
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
    pub release_date: Option<String>,
    pub track_number: Option<u32>,
    pub duration: Option<Duration>,
    pub bytes: u64,
}

pub fn scan(root: &Path) -> (Vec<Track>, usize) {
    let exclusions = load_exclusions(root);
    let mut tracks = Vec::new();
    let mut unreadable = 0;

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !is_excluded(entry.path(), &exclusions))
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

pub fn exclusion_file(root: &Path) -> PathBuf {
    root.join(".mausiker-exclude")
}

fn load_exclusions(root: &Path) -> Vec<PathBuf> {
    let Ok(contents) = fs::read_to_string(exclusion_file(root)) else {
        return Vec::new();
    };
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .collect()
}

fn is_excluded(path: &Path, exclusions: &[PathBuf]) -> bool {
    exclusions.iter().any(|excluded| path.starts_with(excluded))
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
        release_date: tag
            .and_then(|tag| {
                tag.get_string(ItemKey::RecordingDate)
                    .or_else(|| tag.get_string(ItemKey::Year))
            })
            .map(ToOwned::to_owned),
        track_number: tag.and_then(|tag| tag.track()),
        duration: Some(tagged_file.properties().duration()),
        bytes: fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    })
}

pub fn write_metadata(
    path: &Path,
    title: Option<&str>,
    artist: Option<&str>,
    album_artist: Option<&str>,
    album: &str,
    release_date: &str,
) -> Result<(), String> {
    let mut tagged_file = read_from_path(path).map_err(|error| error.to_string())?;
    let tag = tagged_file
        .primary_tag_mut()
        .ok_or_else(|| "the file has no writable primary tag".to_owned())?;

    if let Some(title) = title {
        tag.set_title(title.to_owned());
    }
    if let Some(artist) = artist {
        tag.set_artist(artist.to_owned());
    }
    if let Some(album_artist) = album_artist {
        tag.insert_text(ItemKey::AlbumArtist, album_artist.to_owned());
    }
    tag.set_album(album.to_owned());
    tag.remove_key(ItemKey::Year);
    tag.remove_key(ItemKey::RecordingDate);
    if !release_date.trim().is_empty() {
        tag.insert_text(ItemKey::RecordingDate, release_date.trim().to_owned());
    }

    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| error.to_string())
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

pub fn is_valid_release_date(date: &str) -> bool {
    let parts: Vec<_> = date.split('-').collect();
    if !(1..=3).contains(&parts.len())
        || parts[0].len() != 4
        || (parts.len() >= 2 && parts[1].len() != 2)
        || (parts.len() == 3 && parts[2].len() != 2)
        || !parts
            .iter()
            .all(|part| part.chars().all(|character| character.is_ascii_digit()))
    {
        return false;
    }
    if parts.len() == 1 {
        return true;
    }

    let Ok(month) = parts[1].parse::<u32>() else {
        return false;
    };
    if !(1..=12).contains(&month) || (parts.len() == 2) {
        return (1..=12).contains(&month);
    }

    let (Ok(year), Ok(day)) = (parts[0].parse::<u32>(), parts[2].parse::<u32>()) else {
        return false;
    };
    let days_in_month = match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    };
    (1..=days_in_month).contains(&day)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{is_excluded, is_valid_release_date, main_artist_from_credit};

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

    #[test]
    fn validates_supported_release_date_formats() {
        for date in ["2005", "2005-02", "2005-02-28", "2004-02-29"] {
            assert!(is_valid_release_date(date), "{date} should be valid");
        }
        for date in ["05", "2005-2", "2005-13", "2005-02-29", "2005/02/01"] {
            assert!(!is_valid_release_date(date), "{date} should be invalid");
        }
    }

    #[test]
    fn recognizes_relative_and_absolute_excluded_folders() {
        let root = Path::new("/music");
        let exclusions = vec![root.join("Podcasts"), PathBuf::from("/archive")];
        assert!(is_excluded(
            Path::new("/music/Podcasts/episode.mp3"),
            &exclusions
        ));
        assert!(is_excluded(Path::new("/archive/old.mp3"), &exclusions));
        assert!(!is_excluded(
            Path::new("/music/Albums/track.mp3"),
            &exclusions
        ));
    }
}
