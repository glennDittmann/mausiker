use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use lofty::{
    file::AudioFile,
    prelude::{Accessor, TaggedFileExt},
    read_from_path,
    tag::ItemKey,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use walkdir::WalkDir;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir()?);
    ratatui::run(|terminal| app(terminal, root))?;
    Ok(())
}

fn app(terminal: &mut DefaultTerminal, root: PathBuf) -> io::Result<()> {
    let mut app = App::scan(root);

    loop {
        terminal.draw(|frame| render(frame, &mut app))?;
        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if !key.is_press() {
                continue;
            }
            match key.code {
                crossterm::event::KeyCode::Char('q') | crossterm::event::KeyCode::Esc => {
                    break Ok(());
                }
                crossterm::event::KeyCode::Char('r') => app.rescan(),
                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j') => {
                    app.next();
                }
                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') => {
                    app.previous();
                }
                crossterm::event::KeyCode::Enter | crossterm::event::KeyCode::Char(' ') => {
                    app.toggle_selected_album();
                }
                crossterm::event::KeyCode::Right | crossterm::event::KeyCode::Char('l') => {
                    app.expand_selected_album();
                }
                crossterm::event::KeyCode::Left | crossterm::event::KeyCode::Char('h') => {
                    app.collapse_selected_album();
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug)]
struct Track {
    path: PathBuf,
    title: String,
    artist: String,
    album_artist: String,
    album: String,
    duration: Option<Duration>,
    bytes: u64,
}

#[derive(Debug)]
struct Album {
    artist: String,
    title: String,
    tracks: Vec<Track>,
}

#[derive(Clone, Copy)]
enum LibraryRow {
    Album(usize),
    Track { album: usize, track: usize },
}

struct App {
    root: PathBuf,
    albums: Vec<Album>,
    expanded_albums: BTreeSet<usize>,
    unreadable: usize,
    state: TableState,
}

impl App {
    fn scan(root: PathBuf) -> Self {
        let (tracks, unreadable) = scan_library(&root);
        let mut state = TableState::default();
        let albums = group_by_album(tracks);
        if !albums.is_empty() {
            state.select(Some(0));
        }
        Self {
            root,
            albums,
            expanded_albums: BTreeSet::new(),
            unreadable,
            state,
        }
    }

    fn rescan(&mut self) {
        let (tracks, unreadable) = scan_library(&self.root);
        self.albums = group_by_album(tracks);
        self.expanded_albums.clear();
        self.unreadable = unreadable;
        self.state.select((!self.albums.is_empty()).then_some(0));
    }

    fn next(&mut self) {
        let row_count = self.visible_rows().len();
        if row_count == 0 {
            return;
        }
        let next = self.state.selected().map_or(0, |i| (i + 1) % row_count);
        self.state.select(Some(next));
    }

    fn previous(&mut self) {
        let row_count = self.visible_rows().len();
        if row_count == 0 {
            return;
        }
        let previous = self
            .state
            .selected()
            .map_or(0, |i| if i == 0 { row_count - 1 } else { i - 1 });
        self.state.select(Some(previous));
    }

    fn visible_rows(&self) -> Vec<LibraryRow> {
        self.albums
            .iter()
            .enumerate()
            .flat_map(|(album_index, album)| {
                let mut rows = vec![LibraryRow::Album(album_index)];
                if self.expanded_albums.contains(&album_index) {
                    rows.extend((0..album.tracks.len()).map(|track| LibraryRow::Track {
                        album: album_index,
                        track,
                    }));
                }
                rows
            })
            .collect()
    }

    fn expand_selected_album(&mut self) {
        if let Some(LibraryRow::Album(album)) = self.selected_row() {
            self.expanded_albums.insert(album);
        }
    }

    fn toggle_selected_album(&mut self) {
        if let Some(LibraryRow::Album(album)) = self.selected_row() {
            if !self.expanded_albums.remove(&album) {
                self.expanded_albums.insert(album);
            }
        }
    }

    fn collapse_selected_album(&mut self) {
        if let Some(LibraryRow::Album(album)) = self.selected_row() {
            self.expanded_albums.remove(&album);
        }
    }

    fn selected_row(&self) -> Option<LibraryRow> {
        self.state
            .selected()
            .and_then(|selected| self.visible_rows().get(selected).copied())
    }
}

fn group_by_album(tracks: Vec<Track>) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    for track in tracks {
        if let Some(album) = albums
            .iter_mut()
            .find(|album| album.artist == track.album_artist && album.title == track.album)
        {
            album.tracks.push(track);
        } else {
            albums.push(Album {
                artist: track.album_artist.clone(),
                title: track.album.clone(),
                tracks: vec![track],
            });
        }
    }
    albums
}

fn scan_library(root: &Path) -> (Vec<Track>, usize) {
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
    tracks.sort_by(|a, b| {
        a.artist
            .cmp(&b.artist)
            .then(a.album.cmp(&b.album))
            .then(a.title.cmp(&b.title))
    });
    (tracks, unreadable)
}

fn is_supported_audio(path: &Path) -> bool {
    matches!(path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "mp3" | "m4a" | "mp4" | "aac" | "flac" | "wav" | "ogg" | "opus"))
}

fn read_track(path: &Path) -> lofty::error::Result<Track> {
    let tagged_file = read_from_path(path)?;
    let tag = tagged_file.primary_tag();
    let fallback = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Untitled");
    Ok(Track {
        path: path.to_path_buf(),
        title: tag
            .and_then(|tag| tag.title())
            .map(|value| value.into_owned())
            .unwrap_or_else(|| fallback.to_owned()),
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
        duration: Some(tagged_file.properties().duration()),
        bytes: fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    })
}

fn main_artist_from_credit(artist: Option<&str>) -> String {
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

fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let [header, table_area, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .areas(area);

    let track_count: usize = app.albums.iter().map(|album| album.tracks.len()).sum();
    let total_bytes: u64 = app
        .albums
        .iter()
        .flat_map(|album| &album.tracks)
        .map(|track| track.bytes)
        .sum();
    let title = format!(" Mausiker — {} ", app.root.display());
    let summary = format!(
        "{} albums · {} tracks · {} · {} unreadable",
        app.albums.len(),
        track_count,
        format_bytes(total_bytes),
        app.unreadable
    );
    frame.render_widget(
        Paragraph::new(Line::from(summary))
            .block(Block::default().borders(Borders::ALL).title(title)),
        header,
    );

    let header = Row::new(["Title", "Artist", "Album", "Length", "Format", "Size"]).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.visible_rows().into_iter().map(|row| match row {
        LibraryRow::Album(album_index) => {
            let album = &app.albums[album_index];
            let marker = if app.expanded_albums.contains(&album_index) {
                "▼"
            } else {
                "▶"
            };
            Row::new([
                Cell::from(format!("{marker} {}", album.title)),
                Cell::from(album.artist.clone()),
                Cell::from(format!("{} tracks", album.tracks.len())),
                Cell::from(""),
                Cell::from("ALBUM"),
                Cell::from(format_bytes(
                    album.tracks.iter().map(|track| track.bytes).sum(),
                )),
            ])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        }
        LibraryRow::Track { album, track } => {
            let track = &app.albums[album].tracks[track];
            Row::new([
                Cell::from(format!("  └─ {}", track.title)),
                Cell::from(track.artist.clone()),
                Cell::from(""),
                Cell::from(
                    track
                        .duration
                        .map(format_duration)
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(
                    track
                        .path
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .unwrap_or("?")
                        .to_uppercase(),
                ),
                Cell::from(format_bytes(track.bytes)),
            ])
        }
    });
    let table = Table::new(
        rows,
        [
            Constraint::Fill(3),
            Constraint::Fill(2),
            Constraint::Fill(2),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(9),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" Library "))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(table, table_area, &mut app.state);

    frame.render_widget(
        Paragraph::new("↑/k ↓/j select · Enter toggle · → expand · ← collapse · r rescan · q quit")
            .block(Block::default().borders(Borders::ALL)),
        footer,
    );
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}
