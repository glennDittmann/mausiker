use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

use crossterm::event::{self, KeyCode, KeyModifiers};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};

use crate::{
    conversion::{self, CompletedConversion},
    library::{self, Track},
};

#[derive(Debug)]
struct Album {
    group: Option<String>,
    artist: String,
    title: String,
    tracks: Vec<Track>,
}

struct FolderGroup {
    title: String,
    albums: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    AlbumMetadata,
    Folders,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackFilter {
    NoReleaseDate,
    New,
}

impl TrackFilter {
    const OPTIONS: [Option<Self>; 3] = [None, Some(Self::NoReleaseDate), Some(Self::New)];

    fn label(self) -> &'static str {
        match self {
            Self::NoReleaseDate => "No release date",
            Self::New => "New (last 2 weeks)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryRow {
    Album(usize),
    FolderGroup(usize),
    FolderAlbum(usize),
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

struct Playback {
    path: PathBuf,
    title: String,
    child: Child,
    started_at: Instant,
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
    folder_albums: Vec<Album>,
    folder_groups: Vec<FolderGroup>,
    view_mode: ViewMode,
    expanded_albums: BTreeSet<usize>,
    expanded_folder_groups: BTreeSet<usize>,
    unreadable: usize,
    state: TableState,
    search: Option<String>,
    search_input: Option<String>,
    editor: Option<MetadataEditor>,
    status: Option<String>,
    playback: Option<Playback>,
    conversion_queue: Vec<PathBuf>,
    completed_conversions: Vec<CompletedConversion>,
    delete_confirmation: bool,
    active_filter: Option<TrackFilter>,
    filter_menu: Option<usize>,
    started_at: Instant,
}

pub fn run(terminal: &mut DefaultTerminal, root: PathBuf) -> io::Result<()> {
    let mut app = App::scan(root);
    loop {
        app.refresh_playback();
        terminal.draw(|frame| render(frame, &mut app))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        if let event::Event::Key(key) = event::read()? {
            if !key.is_press() {
                continue;
            }
            if app.delete_confirmation {
                app.handle_delete_confirmation(key.code);
                continue;
            }
            if app.filter_menu.is_some() {
                app.handle_filter_menu_key(key.code);
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
                KeyCode::Char('q') => {
                    app.stop_playback();
                    break Ok(());
                }
                KeyCode::Esc => {
                    if !app.clear_search() {
                        app.stop_playback();
                        break Ok(());
                    }
                }
                KeyCode::Char('r') => app.rescan(),
                KeyCode::Char('v') => app.toggle_view(),
                KeyCode::Char('f') => app.open_filter_menu(),
                KeyCode::Char('c') => app.enqueue_selected(),
                KeyCode::Char('C') => app.run_conversion_queue(),
                KeyCode::Char('d') => app.request_delete_originals(),
                KeyCode::Char('e') => app.open_editor(),
                KeyCode::Down | KeyCode::Char('j') => app.next(),
                KeyCode::Up | KeyCode::Char('k') => app.previous(),
                KeyCode::Enter => app.toggle_selected_album(),
                KeyCode::Char(' ') => app.toggle_playback_selected(),
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
        let albums = group_by_album(tracks.clone());
        let folder_albums = group_by_folder(root.as_path(), tracks);
        let folder_groups = folder_groups(&folder_albums);
        let mut state = TableState::default();
        state.select((!albums.is_empty()).then_some(0));
        Self {
            root,
            albums,
            folder_albums,
            folder_groups,
            view_mode: ViewMode::AlbumMetadata,
            expanded_albums: BTreeSet::new(),
            expanded_folder_groups: BTreeSet::new(),
            unreadable,
            state,
            search: None,
            search_input: None,
            editor: None,
            status: None,
            playback: None,
            conversion_queue: Vec::new(),
            completed_conversions: Vec::new(),
            delete_confirmation: false,
            active_filter: None,
            filter_menu: None,
            started_at: Instant::now(),
        }
    }

    fn rescan(&mut self) {
        let (tracks, unreadable) = library::scan(&self.root);
        self.albums = group_by_album(tracks.clone());
        self.folder_albums = group_by_folder(self.root.as_path(), tracks);
        self.folder_groups = folder_groups(&self.folder_albums);
        self.expanded_albums.clear();
        self.expanded_folder_groups.clear();
        self.unreadable = unreadable;
        self.state
            .select((!self.active_albums().is_empty()).then_some(0));
    }

    fn active_albums(&self) -> &[Album] {
        match self.view_mode {
            ViewMode::AlbumMetadata => &self.albums,
            ViewMode::Folders => &self.folder_albums,
        }
    }

    fn toggle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::AlbumMetadata => ViewMode::Folders,
            ViewMode::Folders => ViewMode::AlbumMetadata,
        };
        self.expanded_albums.clear();
        self.expanded_folder_groups.clear();
        self.state
            .select((!self.active_albums().is_empty()).then_some(0));
        self.status = Some(match self.view_mode {
            ViewMode::AlbumMetadata => "Album metadata view".into(),
            ViewMode::Folders => "Folder view".into(),
        });
    }

    fn open_filter_menu(&mut self) {
        self.filter_menu = Some(
            TrackFilter::OPTIONS
                .iter()
                .position(|filter| *filter == self.active_filter)
                .unwrap_or(0),
        );
    }

    fn handle_filter_menu_key(&mut self, key: KeyCode) {
        let selected = self.filter_menu.as_mut().expect("filter menu is active");
        match key {
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1) % TrackFilter::OPTIONS.len();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = if *selected == 0 {
                    TrackFilter::OPTIONS.len() - 1
                } else {
                    *selected - 1
                };
            }
            KeyCode::Enter => {
                self.active_filter = TrackFilter::OPTIONS[*selected];
                self.filter_menu = None;
                self.expanded_albums.clear();
                self.expanded_folder_groups.clear();
                self.state
                    .select((!self.visible_rows().is_empty()).then_some(0));
                self.status = Some(match self.active_filter {
                    Some(filter) => format!("Filter applied: {}", filter.label()),
                    None => "Filter cleared".into(),
                });
            }
            KeyCode::Esc => self.filter_menu = None,
            _ => {}
        }
    }

    fn enqueue_selected(&mut self) {
        let Some(selected) = self.selected_row() else {
            return;
        };
        let paths: Vec<_> = match selected {
            LibraryRow::Album(album) => self.active_albums()[album]
                .tracks
                .iter()
                .map(|track| track.path.clone())
                .collect(),
            LibraryRow::FolderAlbum(album) => self.folder_albums[album]
                .tracks
                .iter()
                .map(|track| track.path.clone())
                .collect(),
            LibraryRow::FolderGroup(group) => self.folder_groups[group]
                .albums
                .iter()
                .flat_map(|album| self.folder_albums[*album].tracks.iter())
                .map(|track| track.path.clone())
                .collect(),
            LibraryRow::Track { album, track } => {
                vec![self.active_albums()[album].tracks[track].path.clone()]
            }
        };
        let mut queued = 0;
        let mut skipped = 0;
        for path in paths {
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m4a"))
            {
                skipped += 1;
            } else if !self.conversion_queue.contains(&path) {
                self.conversion_queue.push(path);
                queued += 1;
            }
        }
        self.status = Some(if queued > 0 {
            format!("Queued {queued} track(s) for M4A conversion")
        } else if skipped > 0 {
            "M4A files are already in the target format".into()
        } else {
            "Selected tracks are already queued".into()
        });
    }

    fn run_conversion_queue(&mut self) {
        if self.conversion_queue.is_empty() {
            self.status = Some("Conversion queue is empty".into());
            return;
        }
        let queued = std::mem::take(&mut self.conversion_queue);
        let mut failures = Vec::new();
        let mut converted = 0;
        for source in queued {
            match conversion::convert(&self.root, &source) {
                Ok(completed) => {
                    self.completed_conversions.push(completed);
                    converted += 1;
                }
                Err(error) => {
                    failures.push(source);
                    self.status = Some(format!("Conversion error: {error}"));
                }
            }
        }
        self.conversion_queue = failures;
        self.rescan();
        self.status = Some(if self.conversion_queue.is_empty() {
            format!("Converted and verified {converted} track(s); originals kept")
        } else {
            format!(
                "Converted {converted}; {} track(s) remain queued",
                self.conversion_queue.len()
            )
        });
    }

    fn request_delete_originals(&mut self) {
        if self.completed_conversions.is_empty() {
            self.status = Some("No verified conversions are available to delete".into());
            return;
        }
        self.delete_confirmation = true;
    }

    fn handle_delete_confirmation(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let mut deleted = 0;
                let mut retained = Vec::new();
                for completed in std::mem::take(&mut self.completed_conversions) {
                    if conversion::verify_output(&completed.output).is_ok() {
                        match std::fs::remove_file(&completed.original) {
                            Ok(()) => deleted += 1,
                            Err(error) => {
                                self.status = Some(format!(
                                    "Could not delete {}: {error}",
                                    completed.original.display()
                                ));
                                retained.push(completed);
                            }
                        }
                    } else {
                        retained.push(completed);
                    }
                }
                self.completed_conversions = retained;
                self.delete_confirmation = false;
                self.rescan();
                self.status = Some(format!(
                    "Deleted {deleted} original track(s) after verification"
                ));
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.delete_confirmation = false;
                self.status = Some("Original files kept".into());
            }
            _ => {}
        }
    }

    fn open_editor(&mut self) {
        let Some(selected) = self.selected_row() else {
            return;
        };
        let editor = match selected {
            LibraryRow::Album(album_index) => {
                let album = &self.active_albums()[album_index];
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
            LibraryRow::FolderAlbum(album_index) => {
                let album = &self.folder_albums[album_index];
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
            LibraryRow::FolderGroup(_) => {
                self.status =
                    Some("Expand a grouping folder and select an album or track to edit".into());
                return;
            }
            LibraryRow::Track {
                album,
                track: track_index,
            } => {
                let track = &self.active_albums()[album].tracks[track_index];
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
                let paths: Vec<_> = self.active_albums()[album]
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
                let path = self.active_albums()[album].tracks[track].path.clone();
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

    fn refresh_playback(&mut self) {
        let finished = self
            .playback
            .as_mut()
            .is_some_and(|playback| playback.child.try_wait().ok().flatten().is_some());
        if finished {
            self.playback = None;
            self.status = Some("Playback finished".into());
        }
    }

    fn toggle_playback_selected(&mut self) {
        let Some(LibraryRow::Track { album, track }) = self.selected_row() else {
            self.status = Some("Select a track to play it".into());
            return;
        };
        let track = &self.active_albums()[album].tracks[track];
        let path = track.path.clone();
        let title = track.title.clone();
        if self
            .playback
            .as_ref()
            .is_some_and(|playback| playback.path == path)
        {
            self.stop_playback();
            self.status = Some("Playback stopped".into());
            return;
        }

        self.stop_playback();
        match start_playback(&path) {
            Ok(child) => {
                self.playback = Some(Playback {
                    path,
                    title,
                    child,
                    started_at: Instant::now(),
                });
                self.status = None;
            }
            Err(error) => self.status = Some(format!("Could not start playback: {error}")),
        }
    }

    fn stop_playback(&mut self) {
        if let Some(mut playback) = self.playback.take() {
            let _ = playback.child.kill();
            let _ = playback.child.wait();
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
        match self.view_mode {
            ViewMode::AlbumMetadata => self
                .albums
                .iter()
                .enumerate()
                .filter(|(_, album)| self.album_matches(album))
                .flat_map(|(album_index, album)| {
                    let mut rows = vec![LibraryRow::Album(album_index)];
                    if self.expanded_albums.contains(&album_index) {
                        rows.extend(
                            (0..album.tracks.len())
                                .filter(|track| self.track_matches_filter(&album.tracks[*track]))
                                .map(|track| LibraryRow::Track {
                                    album: album_index,
                                    track,
                                }),
                        );
                    }
                    rows
                })
                .collect(),
            ViewMode::Folders => self
                .folder_groups
                .iter()
                .enumerate()
                .filter(|(_, group)| {
                    group
                        .albums
                        .iter()
                        .any(|album| self.album_matches(&self.folder_albums[*album]))
                })
                .flat_map(|(group_index, group)| {
                    let mut rows = vec![LibraryRow::FolderGroup(group_index)];
                    if self.expanded_folder_groups.contains(&group_index) {
                        for album_index in group
                            .albums
                            .iter()
                            .copied()
                            .filter(|album| self.album_matches(&self.folder_albums[*album]))
                        {
                            rows.push(LibraryRow::FolderAlbum(album_index));
                            if self.expanded_albums.contains(&album_index) {
                                rows.extend(
                                    (0..self.folder_albums[album_index].tracks.len())
                                        .filter(|track| {
                                            self.track_matches_filter(
                                                &self.folder_albums[album_index].tracks[*track],
                                            )
                                        })
                                        .map(|track| LibraryRow::Track {
                                            album: album_index,
                                            track,
                                        }),
                                );
                            }
                        }
                    }
                    rows
                })
                .collect(),
        }
    }

    fn album_matches_search(&self, album: &Album) -> bool {
        let Some(search) = &self.search else {
            return true;
        };
        album.title.to_lowercase().contains(search)
            || album.artist.to_lowercase().contains(search)
            || album
                .group
                .as_ref()
                .is_some_and(|group| group.to_lowercase().contains(search))
            || album
                .tracks
                .iter()
                .any(|track| track.artist.to_lowercase().contains(search))
    }

    fn album_matches(&self, album: &Album) -> bool {
        self.album_matches_search(album)
            && album
                .tracks
                .iter()
                .any(|track| self.track_matches_filter(track))
    }

    fn track_matches_filter(&self, track: &Track) -> bool {
        match self.active_filter {
            None => true,
            Some(TrackFilter::NoReleaseDate) => track.release_date.is_none(),
            Some(TrackFilter::New) => track_is_new(track),
        }
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
        match self.selected_row() {
            Some(LibraryRow::Album(album) | LibraryRow::FolderAlbum(album)) => {
                self.expanded_albums.insert(album);
            }
            Some(LibraryRow::FolderGroup(group)) => {
                self.expanded_folder_groups.insert(group);
            }
            _ => {}
        }
    }

    fn toggle_selected_album(&mut self) {
        match self.selected_row() {
            Some(LibraryRow::Album(album) | LibraryRow::FolderAlbum(album)) => {
                if !self.expanded_albums.remove(&album) {
                    self.expanded_albums.insert(album);
                }
            }
            Some(LibraryRow::FolderGroup(group)) => {
                if !self.expanded_folder_groups.remove(&group) {
                    self.expanded_folder_groups.insert(group);
                }
            }
            _ => {}
        }
    }

    fn collapse_selected_album(&mut self) {
        let album = match self.selected_row() {
            Some(
                LibraryRow::Album(album)
                | LibraryRow::FolderAlbum(album)
                | LibraryRow::Track { album, .. },
            ) => album,
            Some(LibraryRow::FolderGroup(group)) => {
                self.expanded_folder_groups.remove(&group);
                return;
            }
            None => return,
        };
        if let Some(album_row) = self
            .visible_rows()
            .iter()
            .position(|row| {
                matches!(row, LibraryRow::Album(index) | LibraryRow::FolderAlbum(index) if *index == album)
            })
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
                group: None,
                artist: track.album_artist.clone(),
                title: track.album.clone(),
                tracks: vec![track],
            });
        }
    }
    sort_album_tracks(&mut albums);
    albums
}

fn group_by_folder(root: &Path, tracks: Vec<Track>) -> Vec<Album> {
    let mut albums: Vec<Album> = Vec::new();
    for track in tracks {
        let album_path = track.path.parent().unwrap_or(root);
        let artist_path = album_path.parent().unwrap_or(root);
        let group_path = artist_path.parent().unwrap_or(root);
        let album_name = folder_name(album_path, "Unknown album folder");
        let artist_name = folder_name(artist_path, "Unknown artist folder");
        let group_name = group_path
            .strip_prefix(root)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.display().to_string());

        if let Some(album) = albums.iter_mut().find(|album| {
            album.group == group_name && album.artist == artist_name && album.title == album_name
        }) {
            album.tracks.push(track);
        } else {
            albums.push(Album {
                group: group_name,
                artist: artist_name,
                title: album_name,
                tracks: vec![track],
            });
        }
    }
    sort_album_tracks(&mut albums);
    albums.sort_by(|left, right| {
        left.group
            .cmp(&right.group)
            .then(left.artist.cmp(&right.artist))
            .then(left.title.cmp(&right.title))
    });
    albums
}

fn folder_groups(albums: &[Album]) -> Vec<FolderGroup> {
    let mut groups: Vec<FolderGroup> = Vec::new();
    for (album_index, album) in albums.iter().enumerate() {
        let title = album.group.clone().unwrap_or_else(|| "Library root".into());
        if let Some(group) = groups.iter_mut().find(|group| group.title == title) {
            group.albums.push(album_index);
        } else {
            groups.push(FolderGroup {
                title,
                albums: vec![album_index],
            });
        }
    }
    groups
}

fn folder_name(path: &Path, fallback: &str) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
        .to_owned()
}

fn sort_album_tracks(albums: &mut [Album]) {
    for album in albums {
        album.tracks.sort_by(|left, right| {
            left.track_number
                .unwrap_or(u32::MAX)
                .cmp(&right.track_number.unwrap_or(u32::MAX))
                .then(left.title.cmp(&right.title))
        });
    }
}

fn track_is_new(track: &Track) -> bool {
    const TWO_WEEKS: Duration = Duration::from_secs(14 * 24 * 60 * 60);
    std::fs::metadata(&track.path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age <= TWO_WEEKS)
}

fn render(frame: &mut Frame, app: &mut App) {
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let view_mode = app.view_mode;
    let (album_count, track_count, total_bytes) = {
        let active_albums = app.active_albums();
        (
            active_albums.len(),
            active_albums
                .iter()
                .map(|album| album.tracks.len())
                .sum::<usize>(),
            active_albums
                .iter()
                .flat_map(|album| &album.tracks)
                .map(|track| track.bytes)
                .sum::<u64>(),
        )
    };
    let summary = format!(
        "{} {} · {} tracks · {} · {} unreadable",
        album_count,
        match view_mode {
            ViewMode::AlbumMetadata => "albums",
            ViewMode::Folders => "folder albums",
        },
        track_count,
        format_bytes(total_bytes),
        app.unreadable
    );
    let mut summary_spans = vec![Span::raw(summary)];
    if let Some(playback) = &app.playback {
        summary_spans.push(Span::raw("  "));
        summary_spans.push(Span::styled(
            " PLAYING ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
        summary_spans.push(Span::styled(
            marquee(&playback.title, 24, playback.started_at.elapsed()),
            Style::default().fg(Color::LightGreen),
        ));
    }
    if !app.conversion_queue.is_empty() {
        summary_spans.push(Span::raw("  "));
        summary_spans.push(Span::styled(
            format!(" QUEUE: {} ", app.conversion_queue.len()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(search) = &app.search {
        let pulse_is_bright = (app.started_at.elapsed().as_millis() / 650).is_multiple_of(2);
        let badge_style = Style::default()
            .fg(Color::Black)
            .bg(if pulse_is_bright {
                Color::Yellow
            } else {
                Color::LightYellow
            })
            .add_modifier(Modifier::BOLD);
        summary_spans.push(Span::raw("  "));
        summary_spans.push(Span::styled(
            format!(" FILTER ACTIVE: {search} "),
            badge_style,
        ));
    }
    if let Some(filter) = app.active_filter {
        summary_spans.push(Span::raw("  "));
        summary_spans.push(Span::styled(
            format!(" FILTER: {} ", filter.label()),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let summary = Line::from(summary_spans);
    frame.render_widget(
        Paragraph::new(summary).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Mausiker — {} ", app.root.display())),
        ),
        header_area,
    );

    let header = Row::new(match view_mode {
        ViewMode::AlbumMetadata => [
            "Title", "Artist", "Album", "Year", "Length", "Format", "Size",
        ],
        ViewMode::Folders => [
            "Album folder",
            "Artist folder",
            "Tracks",
            "Year",
            "Length",
            "Format",
            "Size",
        ],
    })
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    let visible_rows = app.visible_rows();
    let playback = app
        .playback
        .as_ref()
        .map(|playback| (playback.path.clone(), playback.started_at));
    let albums = match view_mode {
        ViewMode::AlbumMetadata => &app.albums,
        ViewMode::Folders => &app.folder_albums,
    };
    let folder_groups = &app.folder_groups;
    let expanded_albums = &app.expanded_albums;
    let expanded_folder_groups = &app.expanded_folder_groups;
    let rows = visible_rows.into_iter().map(|row| match row {
        LibraryRow::FolderGroup(group_index) => {
            let group = &folder_groups[group_index];
            let marker = if expanded_folder_groups.contains(&group_index) {
                "▼"
            } else {
                "▶"
            };
            Row::new([
                Cell::from(format!("{marker} {}", group.title)),
                Cell::from(""),
                Cell::from(format!("{} albums", group.albums.len())),
                Cell::from(""),
                Cell::from(""),
                Cell::from("FOLDER"),
                Cell::from(""),
            ])
            .style(
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )
        }
        LibraryRow::Album(album_index) | LibraryRow::FolderAlbum(album_index) => {
            let album = &albums[album_index];
            let marker = if expanded_albums.contains(&album_index) {
                "▼"
            } else {
                "▶"
            };
            Row::new([
                Cell::from(format!("{marker} {}", album.title)),
                Cell::from(album.artist.clone()),
                Cell::from(match view_mode {
                    ViewMode::AlbumMetadata => format!("{} tracks", album.tracks.len()),
                    ViewMode::Folders => format!("{} tracks", album.tracks.len()),
                }),
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
            let track = &albums[album].tracks[track];
            let playing_indicator = if let Some((playing_path, started_at)) = &playback {
                if playing_path == &track.path {
                    animated_wave(started_at.elapsed())
                } else {
                    "  "
                }
            } else {
                "  "
            };
            Row::new([
                Cell::from(format!(
                    "{playing_indicator}└─ {}  {}",
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
    } else if app.filter_menu.is_some() {
        "Filter menu: ↑/↓ select · Enter apply · Esc cancel".into()
    } else if app.search.is_some() {
        "↑/k ↓/j select · Enter toggle · → expand · ← collapse · Ctrl-K search · Esc clear filter · q quit".into()
    } else if let Some(status) = &app.status {
        format!("{status} · c queue · C convert · d delete originals · q quit")
    } else {
        "↑/k ↓/j select · Enter toggle · Space play/stop · e edit · f filter · c queue · C convert · d delete · v view · Ctrl-K search · r rescan · q quit"
            .into()
    };
    frame.render_widget(
        Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL)),
        footer_area,
    );

    if let Some(editor) = &app.editor {
        render_editor(frame, editor);
    }
    if app.delete_confirmation {
        render_delete_confirmation(frame, app.completed_conversions.len());
    }
    if let Some(selected) = app.filter_menu {
        render_filter_menu(frame, selected);
    }
}

fn render_filter_menu(frame: &mut Frame, selected: usize) {
    let width = 42.min(frame.area().width.saturating_sub(4));
    let height = 9.min(frame.area().height.saturating_sub(4));
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::raw("Choose a track filter:"), Line::raw("")];
    for (index, filter) in TrackFilter::OPTIONS.iter().enumerate() {
        let label = filter.map(TrackFilter::label).unwrap_or("All tracks");
        let style = if index == selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            format!(" {} {label}", if index == selected { ">" } else { " " }),
            style,
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "New uses the file modification time from the last 14 days.",
    ));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Filter ")),
        popup,
    );
}

fn render_delete_confirmation(frame: &mut Frame, count: usize) {
    let width = 64.min(frame.area().width.saturating_sub(4));
    let height = 7.min(frame.area().height.saturating_sub(4));
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(format!(
            "Delete {count} original track(s)?\n\nEach converted M4A will be verified again first.\nThis cannot be undone.  y confirm · Esc keep originals"
        ))
        .block(Block::default().borders(Borders::ALL).title(" Delete originals? ")),
        popup,
    );
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

fn start_playback(path: &Path) -> io::Result<Child> {
    Command::new("ffplay")
        .args(["-nodisp", "-autoexit", "-loglevel", "error"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn animated_wave(elapsed: Duration) -> &'static str {
    const FRAMES: [&str; 4] = ["▁▃▅", "▃▅▃", "▅▃▁", "▃▁▃"];
    FRAMES[(elapsed.as_millis() / 160 % FRAMES.len() as u128) as usize]
}

fn marquee(text: &str, width: usize, elapsed: Duration) -> String {
    let characters: Vec<_> = text.chars().collect();
    let offset = (elapsed.as_millis() / 180 % (characters.len() + width) as u128) as isize
        - characters.len() as isize;
    (0..width)
        .map(|column| {
            let index = column as isize - offset;
            characters.get(index as usize).copied().unwrap_or(' ')
        })
        .collect()
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_playback();
    }
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

    #[test]
    fn marquee_has_a_fixed_width_and_moves_the_title() {
        assert_eq!(
            marquee("A long title", 8, Duration::ZERO).chars().count(),
            8
        );
        assert_ne!(
            marquee("A long title", 8, Duration::ZERO),
            marquee("A long title", 8, Duration::from_millis(2_000))
        );
    }

    #[test]
    fn folder_view_derives_group_artist_and_album_from_paths() {
        let mut first = track(
            "First",
            "Metadata artist",
            "Metadata artist",
            "Metadata album",
            1,
        );
        first.path = PathBuf::from("/music/Rap/50 Cent/The Massacre/01.mp3");
        let folders = group_by_folder(Path::new("/music"), vec![first]);
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].group.as_deref(), Some("Rap"));
        assert_eq!(folders[0].artist, "50 Cent");
        assert_eq!(folders[0].title, "The Massacre");
    }

    #[test]
    fn folder_view_creates_separate_top_level_groups() {
        let mut mine = track("First", "Artist", "Artist", "Album", 1);
        mine.path = PathBuf::from("/music/me/Artist/Album/01.mp3");
        let mut lucas = track("Second", "Artist", "Artist", "Album", 1);
        lucas.path = PathBuf::from("/music/lucas/Artist/Album/01.mp3");
        let albums = group_by_folder(Path::new("/music"), vec![mine, lucas]);
        let groups = folder_groups(&albums);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].title, "lucas");
        assert_eq!(groups[1].title, "me");
    }

    #[test]
    fn no_release_date_filter_keeps_only_matching_tracks() {
        let mut missing_date = track("Missing date", "Artist", "Artist", "Album", 1);
        missing_date.release_date = None;
        let app = App::from_tracks(
            PathBuf::new(),
            vec![
                missing_date,
                track("Has date", "Artist", "Artist", "Album", 2),
            ],
            0,
        );
        let mut app = app;
        app.active_filter = Some(TrackFilter::NoReleaseDate);
        app.expanded_albums.insert(0);
        assert_eq!(
            app.visible_rows(),
            vec![
                LibraryRow::Album(0),
                LibraryRow::Track { album: 0, track: 0 }
            ]
        );
    }
}
