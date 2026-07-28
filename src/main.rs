use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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
    text::{Line, Span},
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
        if !crossterm::event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if !key.is_press() {
                continue;
            }
            if app.search_input.is_some() {
                app.handle_search_key(key.code);
                continue;
            }
            if key.code == crossterm::event::KeyCode::Char('k')
                && key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                app.open_search();
                continue;
            }
            match key.code {
                crossterm::event::KeyCode::Char('q') => {
                    break Ok(());
                }
                crossterm::event::KeyCode::Esc => {
                    if !app.clear_search() {
                        break Ok(());
                    }
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
    track_number: Option<u32>,
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
    search: Option<String>,
    search_input: Option<String>,
    started_at: Instant,
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
            search: None,
            search_input: None,
            started_at: Instant::now(),
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
            .filter(|(_, album)| self.album_matches_search(album))
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

    fn album_matches_search(&self, album: &Album) -> bool {
        let Some(search) = &self.search else {
            return true;
        };
        album.title.to_lowercase().contains(search)
            || album.artist.to_lowercase().contains(search)
            || album
                .tracks
                .iter()
                .any(|track| track.artist.to_lowercase().contains(search))
    }

    fn open_search(&mut self) {
        self.search_input = Some(String::new());
    }

    fn clear_search(&mut self) -> bool {
        if self.search.take().is_none() {
            return false;
        }
        self.state
            .select((!self.visible_rows().is_empty()).then_some(0));
        true
    }

    fn handle_search_key(&mut self, key: crossterm::event::KeyCode) {
        let input = self.search_input.as_mut().expect("search mode is active");
        match key {
            crossterm::event::KeyCode::Char(character) => input.push(character),
            crossterm::event::KeyCode::Backspace => {
                input.pop();
            }
            crossterm::event::KeyCode::Esc => self.search_input = None,
            crossterm::event::KeyCode::Enter => {
                self.search = (!input.trim().is_empty()).then(|| input.trim().to_lowercase());
                self.search_input = None;
                self.state
                    .select((!self.visible_rows().is_empty()).then_some(0));
            }
            _ => {}
        }
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
        let selected_row = self.selected_row();
        let album = match selected_row {
            Some(LibraryRow::Album(album) | LibraryRow::Track { album, .. }) => album,
            None => return,
        };
        if let Some(album_row) = self
            .visible_rows()
            .iter()
            .position(|row| matches!(row, LibraryRow::Album(index) if *index == album))
        {
            self.expanded_albums.remove(&album);
            self.state.select(Some(album_row));
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
    for album in &mut albums {
        album.tracks.sort_by(|left, right| {
            left.track_number
                .unwrap_or(u32::MAX)
                .cmp(&right.track_number.unwrap_or(u32::MAX))
                .then(left.title.cmp(&right.title))
        });
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
        track_number: tag.and_then(|tag| tag.track()),
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
    let summary = if let Some(search) = &app.search {
        let pulse_is_bright = (app.started_at.elapsed().as_millis() / 650).is_multiple_of(2);
        let badge_style = Style::default()
            .fg(Color::Black)
            .bg(if pulse_is_bright {
                Color::Yellow
            } else {
                Color::LightYellow
            })
            .add_modifier(Modifier::BOLD);
        Line::from(vec![
            Span::raw(summary),
            Span::raw("  "),
            Span::styled(format!(" FILTER ACTIVE: {search} "), badge_style),
        ])
    } else {
        Line::from(summary)
    };
    frame.render_widget(
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title(title)),
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
                Cell::from(
                    album_duration(&album.tracks)
                        .map(format_duration)
                        .unwrap_or_else(|| "—".into()),
                ),
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
                Cell::from(format!(
                    "  └─ {}  {}",
                    track
                        .track_number
                        .map(|number| format!("{number:02}"))
                        .unwrap_or_else(|| "--".into()),
                    track.title
                )),
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

    let footer_text = if let Some(input) = &app.search_input {
        format!("Search albums/artists: {input}  · Enter apply · Esc cancel · empty Enter clears")
    } else if app.search.is_some() {
        "↑/k ↓/j select · Enter toggle · → expand · ← collapse · Ctrl-K search · Esc clear filter · q quit".into()
    } else {
        "↑/k ↓/j select · Enter toggle · → expand · ← collapse · Ctrl-K search · r rescan · q quit"
            .into()
    };
    frame.render_widget(
        Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL)),
        footer,
    );
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn album_duration(tracks: &[Track]) -> Option<Duration> {
    tracks
        .iter()
        .filter_map(|track| track.duration)
        .reduce(|total, duration| total + duration)
}

fn format_bytes(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}
