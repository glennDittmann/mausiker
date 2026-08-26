use std::{
    fmt::Write,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, de::DeserializeOwned};

const API_ROOT: &str = "https://musicbrainz.org/ws/2";
const USER_AGENT: &str = "mausiker/0.1.0 (https://github.com/glennDittmann/mausiker)";
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumMatch {
    pub title: String,
    pub artist: String,
    pub release_date: Option<String>,
    pub track_count: Option<u32>,
    pub country: Option<String>,
    pub format: Option<String>,
    pub score: Option<u32>,
    pub genres: Vec<String>,
}

pub fn find_album(title: &str, artist: &str) -> Result<Option<AlbumMatch>, String> {
    let query = format!("release:\"{title}\" AND artist:\"{artist}\"");
    let url = format!(
        "{API_ROOT}/release?query={}&limit=5&fmt=json",
        percent_encode(&query)
    );
    let response: SearchResponse = request_json(&url)?;
    let Some(release) = response
        .releases
        .into_iter()
        .max_by_key(|release| match_score(release, title, artist))
    else {
        return Ok(None);
    };
    let genres = match release
        .release_group
        .as_ref()
        .and_then(|group| group.id.as_deref())
    {
        Some(release_group_id) => find_release_group_genres(release_group_id)?,
        None => Vec::new(),
    };

    Ok(Some(AlbumMatch::from_release(release, genres)))
}

fn find_release_group_genres(release_group_id: &str) -> Result<Vec<String>, String> {
    let url = format!(
        "{API_ROOT}/release-group/{}?inc=genres&fmt=json",
        percent_encode(release_group_id)
    );
    let response: GenreResponse = request_json(&url)?;
    Ok(ranked_genres(response.genres))
}

fn ranked_genres(mut genres: Vec<Genre>) -> Vec<String> {
    genres.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    genres.into_iter().map(|genre| genre.name).collect()
}

fn request_json<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    wait_for_request_slot();
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| format!("MusicBrainz request failed: {error}"))?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| format!("Could not read MusicBrainz response: {error}"))?;
    serde_json::from_str(&body)
        .map_err(|error| format!("Could not parse MusicBrainz response: {error}"))
}

fn wait_for_request_slot() {
    let mut last_request = LAST_REQUEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(delay) = last_request
        .as_ref()
        .and_then(|last| Duration::from_secs(1).checked_sub(last.elapsed()))
    {
        // Holding the lock while waiting also serializes concurrently started lookups.
        thread::sleep(delay);
    }
    *last_request = Some(Instant::now());
}

fn match_score(release: &Release, title: &str, artist: &str) -> u32 {
    let title_match = release.title.eq_ignore_ascii_case(title) as u32 * 10_000;
    let artist_match =
        artist_name(&release.artist_credit).eq_ignore_ascii_case(artist) as u32 * 1_000;
    let release_score = score_value(&release.score).unwrap_or(0);
    title_match + artist_match + release_score
}

impl From<Release> for AlbumMatch {
    fn from(release: Release) -> Self {
        Self::from_release(release, Vec::new())
    }
}

impl AlbumMatch {
    fn from_release(release: Release, genres: Vec<String>) -> Self {
        let score = score_value(&release.score);
        Self {
            title: release.title,
            artist: artist_name(&release.artist_credit),
            release_date: release.date.or(release
                .release_group
                .and_then(|group| group.first_release_date)),
            track_count: release.track_count,
            country: release.country,
            format: release.media.into_iter().find_map(|medium| medium.format),
            score,
            genres,
        }
    }
}

fn score_value(score: &Option<serde_json::Value>) -> Option<u32> {
    match score.as_ref()? {
        serde_json::Value::Number(score) => score.as_u64().and_then(|score| score.try_into().ok()),
        serde_json::Value::String(score) => score.parse().ok(),
        _ => None,
    }
}

fn artist_name(credits: &[ArtistCredit]) -> String {
    credits
        .iter()
        .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
        .collect()
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => write!(encoded, "%{byte:02X}").expect("writing to a string cannot fail"),
        }
    }
    encoded
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Release {
    title: String,
    #[serde(default, rename = "artist-credit")]
    artist_credit: Vec<ArtistCredit>,
    date: Option<String>,
    #[serde(rename = "track-count")]
    track_count: Option<u32>,
    country: Option<String>,
    #[serde(default)]
    media: Vec<Medium>,
    score: Option<serde_json::Value>,
    #[serde(rename = "release-group")]
    release_group: Option<ReleaseGroup>,
}

#[derive(Deserialize)]
struct ArtistCredit {
    name: String,
    #[serde(default)]
    joinphrase: String,
}

#[derive(Deserialize)]
struct Medium {
    format: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseGroup {
    id: Option<String>,
    #[serde(rename = "first-release-date")]
    first_release_date: Option<String>,
}

#[derive(Deserialize)]
struct GenreResponse {
    #[serde(default)]
    genres: Vec<Genre>,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
    #[serde(default)]
    count: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_musicbrainz_search_query() {
        assert_eq!(
            percent_encode("release:\"A & B\""),
            "release%3A%22A%20%26%20B%22"
        );
    }

    #[test]
    fn parses_the_best_release_metadata() {
        let response: SearchResponse = serde_json::from_str(
            r#"{"releases":[{"title":"Illmatic","artist-credit":[{"name":"Nas"}],"date":"1994-04-19","track-count":10,"country":"US","media":[{"format":"CD"}],"score":"100"}]}"#,
        )
        .unwrap();
        let album = AlbumMatch::from(response.releases.into_iter().next().unwrap());

        assert_eq!(album.title, "Illmatic");
        assert_eq!(album.artist, "Nas");
        assert_eq!(album.release_date.as_deref(), Some("1994-04-19"));
        assert_eq!(album.track_count, Some(10));
        assert_eq!(album.format.as_deref(), Some("CD"));
        assert!(album.genres.is_empty());
    }

    #[test]
    fn ranks_release_group_genres_by_vote_count() {
        let response: GenreResponse = serde_json::from_str(
            r#"{"genres":[{"name":"rap","count":3},{"name":"hip hop","count":12},{"name":"east coast hip hop","count":7}]}"#,
        )
        .unwrap();

        assert_eq!(
            ranked_genres(response.genres),
            ["hip hop", "east coast hip hop", "rap"]
        );
    }
}
