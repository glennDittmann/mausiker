use std::{
    collections::BTreeSet,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::event::{self, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};

use crate::library::{self, Track};

#[derive(Debug)]
struct Album {
    artist: String,
    title: String,
    tracks: Vec<Track>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryRow {
    Album(usize),
    Track { album: usize, track: usize },
}

#[derive(Clone, Copy)]
enum EditorTarget {
    Album(usize),
    Track { album: usize, track: usize },
}

struct MetadataEditor {
    target: EditorTarget,
    values: Vec<String>,
    active_field: usize,
}

impl MetadataEditor {
    fn labels(&self) -> &[&str] {
        match self.target {
            EditorTarget::Album(_) => &["Album", "Release date"],
            EditorTarget::Track { .. } => &["Title", "Album", "Release date"],
        }
    }

    fn move_focus(&mut self, direction: isize) {
        let field_count = self.values.len() as isize;
        self.active_field =
            (self.active_field as isize + direction).rem_euclid(field_count) as usize;
    }

    fn release_date(&self) -> &str {
        self.values
            .last()
            .expect("every metadata editor has a release date field")
    }
}

struct App {
    root: PathBuf,
    albums: Vec<Album>,
    expanded_albums: BTreeSet<usize>,
    unreadable: usize,
    state: TableState,
    search: Option<String>,
    search_input: Option<String>,
    editor: Option<MetadataEditor>,
    status: Option<String>,
    started_at: Instant,
}

pub fn run(terminal: &mut DefaultTerminal, root: PathBuf) -> io::Result<()> {
    let mut app = App::scan(root);
    loop {
        terminal.draw(|frame| render(frame, &mut app))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let event::Event::Key(key) = event::read()? {
            if !key.is_press() {
                continue;
            }
            if app.search_input.is_some() {
                app.handle_search_key(key.code);
                continue;
            }
            if app.editor.is_some() {
                app.handle_editor_key(key.code);
                continue;
            }
            if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
                app.open_search();
                continue;
            }
            match key.code {
                KeyCode::Char('q') => break Ok(()),
                KeyCode::Esc => {
                    if !app.clear_search() {
                        break Ok(());
                    }
                }
                KeyCode::Char('r') => app.rescan(),
                KeyCode::Char('e') => app.open_editor(),
                KeyCode::Down | KeyCode::Char('j') => app.next(),
                KeyCode::Up | KeyCode::Char('k') => app.previous(),
                KeyCode::Enter | KeyCode::Char(' ') => app.toggle_selected_album(),
                KeyCode::Right | KeyCode::Char('l') => app.expand_selected_album(),
                KeyCode::Left | KeyCode::Char('h') => app.collapse_selected_album(),
                _ => {}
            }
        }
    }
}

impl App {
    fn scan(root: PathBuf) -> Self {
        let (tracks, unreadable) = library::scan(&root);
        Self::from_tracks(root, tracks, unreadable)
    }

    fn from_tracks(root: PathBuf, tracks: Vec<Track>, unreadable: usize) -> Self {
        let albums = group_by_album(tracks);
        let mut state = TableState::default();
        state.select((!albums.is_empty()).then_some(0));
        Self {
            root,
            albums,
            expanded_albums: BTreeSet::new(),
            unreadable,
            state,
            search: None,
            search_input: None,
            editor: None,
            status: None,
            started_at: Instant::now(),
        }
    }

    fn rescan(&mut self) {
        let (tracks, unreadable) = library::scan(&self.root);
        self.albums = group_by_album(tracks);
        self.expanded_albums.clear();
        self.unreadable = unreadable;
        self.state.select((!self.albums.is_empty()).then_some(0));
    }

    fn open_editor(&mut self) {
        let Some(selected) = self.selected_row() else {
            return;
        };
        let editor = match selected {
            LibraryRow::Album(album_index) => {
                let album = &self.albums[album_index];
                MetadataEditor {
                    target: EditorTarget::Album(album_index),
                    values: vec![
                        album.title.clone(),
                        album
                            .tracks
                            .first()
                            .and_then(|track| track.release_date.clone())
                            .unwrap_or_default(),
                    ],
                    active_field: 0,
                }
            }
            LibraryRow::Track {
                album,
                track: track_index,
            } => {
                let track = &self.albums[album].tracks[track_index];
                MetadataEditor {
                    target: EditorTarget::Track {
                        album,
                        track: track_index,
                    },
                    values: vec![
                        track.title.clone(),
                        track.album.clone(),
                        track.release_date.clone().unwrap_or_default(),
                    ],
                    active_field: 0,
                }
            }
        };
        self.status = None;
        self.editor = Some(editor);
    }

    fn handle_editor_key(&mut self, key: KeyCode) {
        if key == KeyCode::Enter {
            self.save_editor();
            return;
        }
        let editor = self.editor.as_mut().expect("editor is active");
        match key {
            KeyCode::Char(character) => editor.values[editor.active_field].push(character),
            KeyCode::Backspace => {
                editor.values[editor.active_field].pop();
            }
            KeyCode::Tab | KeyCode::Down => editor.move_focus(1),
            KeyCode::BackTab | KeyCode::Up => editor.move_focus(-1),
            KeyCode::Esc => self.editor = None,
            _ => {}
        }
    }

    fn save_editor(&mut self) {
        let editor = self.editor.take().expect("editor is active");
        if !library::is_valid_release_date(editor.release_date()) {
            self.editor = Some(editor);
            return;
        }
        let result = match editor.target {
            EditorTarget::Album(album) => {
                let paths: Vec<_> = self.albums[album]
                    .tracks
                    .iter()
                    .map(|track| track.path.clone())
                    .collect();
                let album_name = &editor.values[0];
                let release_date = &editor.values[1];
                let failures: Vec<_> = paths
                    .iter()
                    .filter_map(|path| {
                        library::write_metadata(path, None, album_name, release_date).err()
                    })
                    .collect();
                if failures.is_empty() {
                    format!("Saved album metadata for {} tracks", paths.len())
                } else {
                    format!(
                        "Saved with {} write error(s): {}",
                        failures.len(),
                        failures[0]
                    )
                }
            }
            EditorTarget::Track { album, track } => {
                let path = self.albums[album].tracks[track].path.clone();
                match library::write_metadata(
                    &path,
                    Some(&editor.values[0]),
                    &editor.values[1],
                    &editor.values[2],
                ) {
                    Ok(()) => "Saved track metadata".into(),
                    Err(error) => format!("Could not save: {error}"),
                }
            }
        };
        self.rescan();
        self.status = Some(result);
    }

    fn next(&mut self) {
        let row_count = self.visible_rows().len();
        if row_count > 0 {
            self.state.select(Some(
                self.state
                    .selected()
                    .map_or(0, |index| (index + 1) % row_count),
            ));
        }
    }

    fn previous(&mut self) {
        let row_count = self.visible_rows().len();
        if row_count > 0 {
            self.state
                .select(Some(self.state.selected().map_or(0, |index| {
                    if index == 0 { row_count - 1 } else { index - 1 }
                })));
        }
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

    fn handle_search_key(&mut self, key: KeyCode) {
        let input = self.search_input.as_mut().expect("search mode is active");
        match key {
            KeyCode::Char(character) => input.push(character),
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Esc => self.search_input = None,
            KeyCode::Enter => {
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
        let album = match self.selected_row() {
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

fn render(frame: &mut Frame, app: &mut App) {
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let track_count: usize = app.albums.iter().map(|album| album.tracks.len()).sum();
    let total_bytes: u64 = app
        .albums
        .iter()
        .flat_map(|album| &album.tracks)
        .map(|track| track.bytes)
        .sum();
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
        Paragraph::new(summary).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Mausiker — {} ", app.root.display())),
        ),
        header_area,
    );

    let header = Row::new([
        "Title", "Artist", "Album", "Year", "Length", "Format", "Size",
    ])
    .style(
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
                    album
                        .tracks
                        .first()
                        .and_then(|track| track.release_date.as_deref())
                        .map(format_release_year)
                        .unwrap_or("—"),
                ),
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
                        .release_date
                        .as_deref()
                        .map(format_release_year)
                        .unwrap_or("—"),
                ),
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
                        .and_then(|extension| extension.to_str())
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
            Constraint::Length(6),
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
    } else if let Some(status) = &app.status {
        format!("{status} · e edit · Ctrl-K search · q quit")
    } else {
        "↑/k ↓/j select · Enter toggle · → expand · ← collapse · e edit · Ctrl-K search · r rescan · q quit"
            .into()
    };
    frame.render_widget(
        Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL)),
        footer_area,
    );

    if let Some(editor) = &app.editor {
        render_editor(frame, editor);
    }
}

fn render_editor(frame: &mut Frame, editor: &MetadataEditor) {
    let width = 70.min(frame.area().width.saturating_sub(4));
    let height = 9.min(frame.area().height.saturating_sub(4));
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let date_is_valid = library::is_valid_release_date(editor.release_date());
    let mut lines = vec![
        Line::raw("Tab/↑/↓ switch field · Enter save · Esc cancel"),
        Line::raw(""),
    ];
    for (index, (label, value)) in editor.labels().iter().zip(&editor.values).enumerate() {
        let marker = if index == editor.active_field {
            ">"
        } else {
            " "
        };
        let style = if *label == "Release date" && !date_is_valid {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::styled(format!("{marker} {label}: {value}"), style));
    }
    if !date_is_valid {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Invalid date — use YYYY, YYYY-MM, or YYYY-MM-DD. Enter is disabled.",
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Edit metadata "),
        ),
        popup,
    );
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn format_release_year(date: &str) -> &str {
    date.get(..4)
        .filter(|year| year.chars().all(|character| character.is_ascii_digit()))
        .unwrap_or(date)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(
        title: &str,
        artist: &str,
        album_artist: &str,
        album: &str,
        track_number: u32,
    ) -> Track {
        Track {
            path: PathBuf::from(format!("{track_number}.mp3")),
            title: title.into(),
            artist: artist.into(),
            album_artist: album_artist.into(),
            album: album.into(),
            release_date: Some("2005".into()),
            track_number: Some(track_number),
            duration: Some(Duration::from_secs(180)),
            bytes: 1,
        }
    }

    #[test]
    fn groups_an_album_and_sorts_its_tracks_by_number() {
        let albums = group_by_album(vec![
            track("Second", "Artist", "Artist", "Album", 2),
            track("First", "Artist", "Artist", "Album", 1),
        ]);
        assert_eq!(albums.len(), 1);
        assert_eq!(
            albums[0]
                .tracks
                .iter()
                .map(|track| track.title.as_str())
                .collect::<Vec<_>>(),
            ["First", "Second"]
        );
    }

    #[test]
    fn search_matches_featured_track_artists() {
        let app = App::from_tracks(
            PathBuf::new(),
            vec![track(
                "Patiently Waiting",
                "50 Cent feat. Eminem",
                "50 Cent",
                "The Massacre",
                1,
            )],
            0,
        );
        let mut app = app;
        app.search = Some("eminem".into());
        assert_eq!(app.visible_rows(), vec![LibraryRow::Album(0)]);
    }

    #[test]
    fn collapsing_from_a_track_selects_its_album_row() {
        let mut app = App::from_tracks(
            PathBuf::new(),
            vec![track("First", "Artist", "Artist", "Album", 1)],
            0,
        );
        app.expanded_albums.insert(0);
        app.state.select(Some(1));
        app.collapse_selected_album();
        assert!(app.expanded_albums.is_empty());
        assert_eq!(app.state.selected(), Some(0));
    }
}
