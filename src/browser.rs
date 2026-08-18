use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossterm::{
    cursor::SetCursorStyle,
    event::{self, KeyCode, KeyModifiers},
    execute,
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::Line,
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

enum BrowserSelection {
    Album(PathBuf),
    Group(String),
    Track(PathBuf),
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
    cursor_positions: Vec<usize>,
}

struct Playback {
    path: PathBuf,
    title: String,
    child: Child,
    started_at: Instant,
}

struct ConversionProgress {
    total: usize,
    processed: usize,
    successful: usize,
    unsuccessful: usize,
    current_path: Option<PathBuf>,
    started_at: Instant,
}

enum ConversionUpdate {
    Started(PathBuf),
    Finished {
        source: PathBuf,
        result: Result<CompletedConversion, String>,
    },
}

struct ConversionPreview {
    plans: Vec<ConversionPlan>,
    scroll: usize,
}

struct ConversionPlan {
    source: PathBuf,
    output: Option<PathBuf>,
    status: ConversionPlanStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversionPlanStatus {
    Ready,
    AlreadyM4a,
    OutputExists,
    OutsideLibrary,
}

impl ConversionPreview {
    fn ready_count(&self) -> usize {
        self.plans
            .iter()
            .filter(|plan| plan.status == ConversionPlanStatus::Ready)
            .count()
    }

    fn skipped_count(&self) -> usize {
        self.plans.len() - self.ready_count()
    }
}

struct ConversionFailure {
    source: PathBuf,
    error: String,
}

struct ConversionResult {
    successful: Vec<CompletedConversion>,
    failures: Vec<ConversionFailure>,
    remaining_queued: usize,
    scroll: usize,
}

impl ConversionResult {
    fn total(&self) -> usize {
        self.successful.len() + self.failures.len()
    }
}

struct RenamePreview {
    plans: Vec<RenamePlan>,
    scroll: usize,
}

struct RenamePlan {
    source: PathBuf,
    target: Option<PathBuf>,
    status: RenameStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenameStatus {
    Ready,
    Unchanged,
    MissingTrackNumber,
    Conflict,
}

impl RenamePreview {
    fn ready_count(&self) -> usize {
        self.plans
            .iter()
            .filter(|plan| plan.status == RenameStatus::Ready)
            .count()
    }

    fn skipped_count(&self) -> usize {
        self.plans.len() - self.ready_count()
    }
}

impl MetadataEditor {
    fn labels(&self) -> &[&str] {
        match self.target {
            EditorTarget::Album(_) => &["Album artist", "Album", "Release date"],
            EditorTarget::Track { .. } => &["Title", "Artist", "Album", "Release date"],
        }
    }

    fn move_focus(&mut self, direction: isize) {
        let field_count = self.values.len() as isize;
        self.active_field =
            (self.active_field as isize + direction).rem_euclid(field_count) as usize;
    }

    fn insert(&mut self, character: char) {
        let field = self.active_field;
        let cursor = self.cursor_positions[field];
        self.values[field].insert(cursor, character);
        self.cursor_positions[field] += character.len_utf8();
    }

    fn move_cursor_left(&mut self) {
        let field = self.active_field;
        let value = &self.values[field];
        self.cursor_positions[field] = value[..self.cursor_positions[field]]
            .char_indices()
            .last()
            .map_or(0, |(index, _)| index);
    }

    fn move_cursor_right(&mut self) {
        let field = self.active_field;
        let value = &self.values[field];
        let cursor = self.cursor_positions[field];
        self.cursor_positions[field] = value[cursor..]
            .chars()
            .next()
            .map_or(cursor, |character| cursor + character.len_utf8());
    }

    fn backspace(&mut self) {
        let field = self.active_field;
        let cursor = self.cursor_positions[field];
        if cursor == 0 {
            return;
        }
        let start = self.values[field][..cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .expect("a non-empty prefix has a previous character");
        self.values[field].drain(start..cursor);
        self.cursor_positions[field] = start;
    }

    fn delete(&mut self) {
        let field = self.active_field;
        let cursor = self.cursor_positions[field];
        let Some(character) = self.values[field][cursor..].chars().next() else {
            return;
        };
        self.values[field].drain(cursor..cursor + character.len_utf8());
    }

    fn cursor(&self) -> usize {
        self.cursor_positions[self.active_field]
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
    path_inspector: Option<Vec<PathBuf>>,
    status: Option<String>,
    playback: Option<Playback>,
    conversion_queue: Vec<PathBuf>,
    conversion_preview: Option<ConversionPreview>,
    conversion_progress: Option<ConversionProgress>,
    conversion_receiver: Option<Receiver<ConversionUpdate>>,
    conversion_successes: Vec<CompletedConversion>,
    conversion_failures: Vec<ConversionFailure>,
    conversion_result: Option<ConversionResult>,
    completed_conversions: Vec<CompletedConversion>,
    rename_preview: Option<RenamePreview>,
    delete_confirmation: Option<usize>,
    active_filter: Option<TrackFilter>,
    filter_menu: Option<usize>,
    started_at: Instant,
}

pub fn run(terminal: &mut DefaultTerminal, root: PathBuf) -> io::Result<()> {
    let mut app = App::scan(root);
    execute!(io::stdout(), SetCursorStyle::BlinkingBar)?;
    let result = (|| {
        loop {
            app.refresh_playback();
            app.refresh_conversion();
            terminal.draw(|frame| render(frame, &mut app))?;
            if !event::poll(Duration::from_millis(200))? {
                continue;
            }
            if let event::Event::Key(key) = event::read()? {
                if !key.is_press() {
                    continue;
                }
                if app.delete_confirmation.is_some() {
                    app.handle_delete_confirmation(key.code);
                    continue;
                }
                if app.rename_preview.is_some() {
                    app.handle_rename_confirmation(key.code);
                    continue;
                }
                if app.conversion_preview.is_some() {
                    app.handle_conversion_preview(key.code);
                    continue;
                }
                if app.conversion_result.is_some() {
                    app.handle_conversion_result(key.code);
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
                if app.path_inspector.is_some() {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('i')) {
                        app.path_inspector = None;
                    }
                    continue;
                }
                if app.conversion_progress.is_some() {
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
                    KeyCode::Char('r') => app.rename_selected(),
                    KeyCode::Char('i') => app.open_path_inspector(),
                    KeyCode::Char('v') => app.toggle_view(),
                    KeyCode::Char('f') => app.open_filter_menu(),
                    KeyCode::Char('c') => app.enqueue_selected(),
                    KeyCode::Char('C') => app.open_conversion_preview(),
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
    })();
    let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape);
    result
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
            path_inspector: None,
            status: None,
            playback: None,
            conversion_queue: Vec::new(),
            conversion_preview: None,
            conversion_progress: None,
            conversion_receiver: None,
            conversion_successes: Vec::new(),
            conversion_failures: Vec::new(),
            conversion_result: None,
            completed_conversions: Vec::new(),
            rename_preview: None,
            delete_confirmation: None,
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

    fn rescan_preserving_browser_state(&mut self) {
        let expanded_album_paths: BTreeSet<_> = self
            .expanded_albums
            .iter()
            .filter_map(|&index| {
                self.active_albums()
                    .get(index)
                    .and_then(|album| album.tracks.first())
                    .map(|track| track.path.clone())
            })
            .collect();
        let expanded_group_titles: BTreeSet<_> = self
            .expanded_folder_groups
            .iter()
            .filter_map(|&index| {
                self.folder_groups
                    .get(index)
                    .map(|group| group.title.clone())
            })
            .collect();
        let selected = match self.selected_row() {
            Some(LibraryRow::Album(index) | LibraryRow::FolderAlbum(index)) => self
                .active_albums()
                .get(index)
                .and_then(|album| album.tracks.first())
                .map(|track| BrowserSelection::Album(track.path.clone())),
            Some(LibraryRow::FolderGroup(index)) => self
                .folder_groups
                .get(index)
                .map(|group| BrowserSelection::Group(group.title.clone())),
            Some(LibraryRow::Track { album, track }) => self
                .active_albums()
                .get(album)
                .and_then(|album| album.tracks.get(track))
                .map(|track| BrowserSelection::Track(track.path.clone())),
            None => None,
        };

        let (tracks, unreadable) = library::scan(&self.root);
        self.albums = group_by_album(tracks.clone());
        self.folder_albums = group_by_folder(self.root.as_path(), tracks);
        self.folder_groups = folder_groups(&self.folder_albums);
        self.unreadable = unreadable;
        self.expanded_albums = self
            .active_albums()
            .iter()
            .enumerate()
            .filter_map(|(index, album)| {
                album
                    .tracks
                    .first()
                    .is_some_and(|track| expanded_album_paths.contains(&track.path))
                    .then_some(index)
            })
            .collect();
        self.expanded_folder_groups = match self.view_mode {
            ViewMode::AlbumMetadata => BTreeSet::new(),
            ViewMode::Folders => self
                .folder_groups
                .iter()
                .enumerate()
                .filter_map(|(index, group)| {
                    expanded_group_titles
                        .contains(&group.title)
                        .then_some(index)
                })
                .collect(),
        };
        let selected_row = self
            .visible_rows()
            .iter()
            .position(|row| match (row, &selected) {
                (
                    LibraryRow::Album(index) | LibraryRow::FolderAlbum(index),
                    Some(BrowserSelection::Album(path)),
                ) => self
                    .active_albums()
                    .get(*index)
                    .and_then(|album| album.tracks.first())
                    .is_some_and(|track| &track.path == path),
                (LibraryRow::FolderGroup(index), Some(BrowserSelection::Group(title))) => self
                    .folder_groups
                    .get(*index)
                    .is_some_and(|group| &group.title == title),
                (LibraryRow::Track { album, track }, Some(BrowserSelection::Track(path))) => self
                    .active_albums()
                    .get(*album)
                    .and_then(|album| album.tracks.get(*track))
                    .is_some_and(|track| &track.path == path),
                _ => false,
            });
        self.state
            .select(selected_row.or_else(|| (!self.visible_rows().is_empty()).then_some(0)));
    }

    fn active_albums(&self) -> &[Album] {
        match self.view_mode {
            ViewMode::AlbumMetadata => &self.albums,
            ViewMode::Folders => &self.folder_albums,
        }
    }

    fn visible_library_totals(&self) -> (usize, usize, u64) {
        self.active_albums()
            .iter()
            .filter(|album| self.album_matches(album))
            .fold((0, 0, 0), |(albums, tracks, bytes), album| {
                let matching_tracks = album
                    .tracks
                    .iter()
                    .filter(|track| self.track_matches_filter(track));
                let track_count = matching_tracks.clone().count();
                let track_bytes = matching_tracks.map(|track| track.bytes).sum::<u64>();
                (albums + 1, tracks + track_count, bytes + track_bytes)
            })
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
        if let LibraryRow::Track { album, track } = selected {
            let path = self.active_albums()[album].tracks[track].path.clone();
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m4a"))
            {
                self.status = Some("M4A files are already in the target format".into());
            } else if let Some(index) = self
                .conversion_queue
                .iter()
                .position(|queued| *queued == path)
            {
                self.conversion_queue.remove(index);
                self.status = Some("Removed track from the conversion queue".into());
            } else {
                self.conversion_queue.push(path);
                self.status = Some("Queued track for M4A conversion".into());
            }
            self.next();
            return;
        }
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
            LibraryRow::Track { .. } => unreachable!("track rows are handled above"),
        };
        let eligible_paths: Vec<_> = paths
            .into_iter()
            .filter(|path| {
                !path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("m4a"))
            })
            .collect();
        if eligible_paths.is_empty() {
            self.status = Some("M4A files are already in the target format".into());
            return;
        }
        if eligible_paths
            .iter()
            .all(|path| self.conversion_queue.contains(path))
        {
            self.conversion_queue
                .retain(|queued| !eligible_paths.contains(queued));
            self.status = Some(format!(
                "Removed {} track(s) from the conversion queue",
                eligible_paths.len()
            ));
            return;
        }

        let mut queued = 0;
        let mut already_queued = 0;
        for path in eligible_paths {
            if self.conversion_queue.contains(&path) {
                already_queued += 1;
            } else {
                self.conversion_queue.push(path);
                queued += 1;
            }
        }
        self.status = Some(if already_queued == 0 {
            format!("Queued {queued} track(s) for M4A conversion")
        } else {
            format!("Queued {queued} track(s); {already_queued} already queued")
        });
    }

    fn selected_tracks(&self) -> Option<Vec<Track>> {
        match self.selected_row()? {
            LibraryRow::Album(album) => Some(self.active_albums()[album].tracks.clone()),
            LibraryRow::FolderAlbum(album) => Some(self.folder_albums[album].tracks.clone()),
            LibraryRow::Track { album, track } => {
                Some(vec![self.active_albums()[album].tracks[track].clone()])
            }
            LibraryRow::FolderGroup(_) => None,
        }
    }

    fn open_path_inspector(&mut self) {
        let Some(tracks) = self.selected_tracks() else {
            self.status = Some("Select an album or track to inspect its file path".into());
            return;
        };
        self.path_inspector = Some(tracks.into_iter().map(|track| track.path).collect());
    }

    fn rename_selected(&mut self) {
        let Some(tracks) = self.selected_tracks() else {
            self.status = Some("Select an album or track to rename its file(s)".into());
            return;
        };
        self.status = None;
        self.rename_preview = Some(build_rename_preview(tracks));
    }

    fn handle_rename_confirmation(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.apply_rename_preview(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.rename_preview = None;
                self.status = Some("Rename cancelled".into());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let preview = self
                    .rename_preview
                    .as_mut()
                    .expect("a rename preview is active while it is navigated");
                preview.scroll = (preview.scroll + 1).min(preview.plans.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let preview = self
                    .rename_preview
                    .as_mut()
                    .expect("a rename preview is active while it is navigated");
                preview.scroll = preview.scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn apply_rename_preview(&mut self) {
        let preview = self
            .rename_preview
            .take()
            .expect("a rename preview is active before it can be applied");
        let mut renamed = 0;
        let mut skipped = preview.skipped_count();
        let mut renamed_paths = Vec::new();
        for plan in preview.plans {
            if plan.status != RenameStatus::Ready {
                continue;
            }
            let target = plan
                .target
                .expect("ready rename plans always include a target path");
            match std::fs::rename(&plan.source, &target) {
                Ok(()) => {
                    renamed_paths.push((plan.source, target));
                    renamed += 1;
                }
                Err(_) => skipped += 1,
            }
        }
        self.update_renamed_paths(&renamed_paths);
        if renamed > 0 {
            self.rescan_preserving_browser_state();
        }
        self.status = Some(match (renamed, skipped) {
            (0, 0) => "File names already match the track numbers and titles".into(),
            (0, skipped) => format!("No tracks renamed; skipped {skipped}"),
            (renamed, 0) => format!("Renamed {renamed} track(s)"),
            (renamed, skipped) => format!("Renamed {renamed} track(s); skipped {skipped}"),
        });
    }

    fn update_renamed_paths(&mut self, renamed_paths: &[(PathBuf, PathBuf)]) {
        for (source, target) in renamed_paths {
            for album in &mut self.albums {
                for track in &mut album.tracks {
                    if track.path == *source {
                        track.path = target.clone();
                    }
                }
            }
            for album in &mut self.folder_albums {
                for track in &mut album.tracks {
                    if track.path == *source {
                        track.path = target.clone();
                    }
                }
            }
            for path in &mut self.conversion_queue {
                if path == source {
                    *path = target.clone();
                }
            }
            for completed in &mut self.completed_conversions {
                if completed.original == *source {
                    completed.original = target.clone();
                }
            }
            if let Some(playback) = &mut self.playback
                && playback.path == *source
            {
                playback.path = target.clone();
            }
        }
    }

    fn open_conversion_preview(&mut self) {
        if self.conversion_queue.is_empty() {
            self.status = Some("Conversion queue is empty".into());
            return;
        }
        self.status = None;
        self.conversion_preview =
            Some(build_conversion_preview(&self.root, &self.conversion_queue));
    }

    fn handle_conversion_preview(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('y') | KeyCode::Char('Y') => self.start_previewed_conversions(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.conversion_preview = None;
                self.status = Some("Conversion cancelled".into());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let preview = self
                    .conversion_preview
                    .as_mut()
                    .expect("a conversion preview is active while it is navigated");
                preview.scroll = (preview.scroll + 1).min(preview.plans.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let preview = self
                    .conversion_preview
                    .as_mut()
                    .expect("a conversion preview is active while it is navigated");
                preview.scroll = preview.scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn start_previewed_conversions(&mut self) {
        let preview = self
            .conversion_preview
            .take()
            .expect("a conversion preview is active before it can be applied");
        let queued: Vec<_> = preview
            .plans
            .iter()
            .filter(|plan| plan.status == ConversionPlanStatus::Ready)
            .map(|plan| plan.source.clone())
            .collect();
        self.conversion_queue = preview
            .plans
            .into_iter()
            .filter(|plan| plan.status != ConversionPlanStatus::Ready)
            .map(|plan| plan.source)
            .collect();
        if queued.is_empty() {
            self.status = Some("No queued tracks can be converted; conflicts remain queued".into());
            return;
        }
        self.start_conversion_queue(queued);
    }

    fn start_conversion_queue(&mut self, queued: Vec<PathBuf>) {
        let total = queued.len();
        let root = self.root.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for source in queued {
                if sender
                    .send(ConversionUpdate::Started(source.clone()))
                    .is_err()
                {
                    break;
                }
                let result = conversion::convert(&root, &source);
                if sender
                    .send(ConversionUpdate::Finished { source, result })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.conversion_progress = Some(ConversionProgress {
            total,
            processed: 0,
            successful: 0,
            unsuccessful: 0,
            current_path: None,
            started_at: Instant::now(),
        });
        self.conversion_receiver = Some(receiver);
        self.conversion_successes.clear();
        self.conversion_failures.clear();
        self.conversion_result = None;
        self.status = None;
    }

    fn refresh_conversion(&mut self) {
        let Some(receiver) = &self.conversion_receiver else {
            return;
        };
        let mut results = Vec::new();
        let mut finished = false;
        loop {
            match receiver.try_recv() {
                Ok(result) => results.push(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        for result in results {
            let progress = self
                .conversion_progress
                .as_mut()
                .expect("conversion progress exists while receiving results");
            match result {
                ConversionUpdate::Started(path) => progress.current_path = Some(path),
                ConversionUpdate::Finished { source, result } => {
                    progress.processed += 1;
                    progress.current_path = None;
                    match result {
                        Ok(completed) => {
                            self.completed_conversions.push(completed.clone());
                            self.conversion_successes.push(completed);
                            progress.successful += 1;
                        }
                        Err(error) => {
                            self.conversion_queue.push(source.clone());
                            self.conversion_failures
                                .push(ConversionFailure { source, error });
                            progress.unsuccessful += 1;
                        }
                    }
                }
            }
        }
        if finished {
            let progress = self
                .conversion_progress
                .take()
                .expect("conversion progress exists when worker finishes");
            self.conversion_receiver = None;
            self.rescan();
            self.conversion_result = Some(ConversionResult {
                successful: std::mem::take(&mut self.conversion_successes),
                failures: std::mem::take(&mut self.conversion_failures),
                remaining_queued: self.conversion_queue.len(),
                scroll: 0,
            });
            self.status = Some(format!(
                "Converted and verified {} track(s); {} remain queued",
                progress.successful,
                self.conversion_queue.len()
            ));
        }
    }

    fn handle_conversion_result(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('C') => self.conversion_result = None,
            KeyCode::Down | KeyCode::Char('j') => {
                let result = self
                    .conversion_result
                    .as_mut()
                    .expect("a conversion result is active while it is navigated");
                result.scroll = (result.scroll + 1).min(result.total().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let result = self
                    .conversion_result
                    .as_mut()
                    .expect("a conversion result is active while it is navigated");
                result.scroll = result.scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn request_delete_originals(&mut self) {
        if self.completed_conversions.is_empty() {
            self.status = Some("No verified conversions are available to delete".into());
            return;
        }
        self.delete_confirmation = Some(0);
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
                self.delete_confirmation = None;
                self.rescan();
                self.status = Some(format!(
                    "Deleted {deleted} original track(s) after verification"
                ));
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.delete_confirmation = None;
                self.status = Some("Original files kept".into());
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let scroll = self
                    .delete_confirmation
                    .as_mut()
                    .expect("a deletion preview is active while it is navigated");
                *scroll = (*scroll + 1).min(self.completed_conversions.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let scroll = self
                    .delete_confirmation
                    .as_mut()
                    .expect("a deletion preview is active while it is navigated");
                *scroll = scroll.saturating_sub(1);
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
                        album.artist.clone(),
                        album.title.clone(),
                        album
                            .tracks
                            .first()
                            .and_then(|track| track.release_date.clone())
                            .unwrap_or_default(),
                    ],
                    active_field: 0,
                    cursor_positions: vec![
                        album.artist.len(),
                        album.title.len(),
                        album
                            .tracks
                            .first()
                            .and_then(|track| track.release_date.as_ref())
                            .map_or(0, String::len),
                    ],
                }
            }
            LibraryRow::FolderAlbum(album_index) => {
                let album = &self.folder_albums[album_index];
                MetadataEditor {
                    target: EditorTarget::Album(album_index),
                    values: vec![
                        album.artist.clone(),
                        album.title.clone(),
                        album
                            .tracks
                            .first()
                            .and_then(|track| track.release_date.clone())
                            .unwrap_or_default(),
                    ],
                    active_field: 0,
                    cursor_positions: vec![
                        album.artist.len(),
                        album.title.len(),
                        album
                            .tracks
                            .first()
                            .and_then(|track| track.release_date.as_ref())
                            .map_or(0, String::len),
                    ],
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
                        track.artist.clone(),
                        track.album.clone(),
                        track.release_date.clone().unwrap_or_default(),
                    ],
                    active_field: 0,
                    cursor_positions: vec![
                        track.title.len(),
                        track.artist.len(),
                        track.album.len(),
                        track.release_date.as_ref().map_or(0, String::len),
                    ],
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
            KeyCode::Char(character) => editor.insert(character),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::Left => editor.move_cursor_left(),
            KeyCode::Right => editor.move_cursor_right(),
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
                let album_artist = &editor.values[0];
                let album_name = &editor.values[1];
                let release_date = &editor.values[2];
                let failures: Vec<_> = paths
                    .iter()
                    .filter_map(|path| {
                        library::write_metadata(
                            path,
                            None,
                            None,
                            Some(album_artist),
                            album_name,
                            release_date,
                        )
                        .err()
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
                    Some(&editor.values[1]),
                    None,
                    &editor.values[2],
                    &editor.values[3],
                ) {
                    Ok(()) => "Saved track metadata".into(),
                    Err(error) => format!("Could not save: {error}"),
                }
            }
        };
        self.rescan_preserving_browser_state();
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
            Some(LibraryRow::FolderGroup(group)) if !self.expanded_folder_groups.remove(&group) => {
                self.expanded_folder_groups.insert(group);
            }
            Some(LibraryRow::FolderGroup(_)) => {}
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
        if let Some(album) = albums.iter_mut().find(|album| album.title == track.album) {
            if album.artist != track.album_artist {
                album.artist = "Various Artists".into();
            }
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

fn truncate_to_width(text: &str, width: usize) -> String {
    if Line::raw(text).width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }
    let mut shortened = String::new();
    for character in text.chars() {
        let character_width = Line::raw(character.to_string()).width();
        if Line::raw(&shortened).width() + character_width + 1 > width {
            break;
        }
        shortened.push(character);
    }
    shortened.push('…');
    shortened
}

fn compact_header_path(path: &Path, width: usize) -> String {
    let compact = compact_preview_path(path);
    if Line::raw(&compact).width() <= width {
        return compact;
    }
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("Library"));
    truncate_to_width(&format!("…/{filename}"), width)
}

fn wrap_header_tokens(tokens: Vec<String>, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for token in tokens {
        let token = truncate_to_width(&token, width);
        let candidate = if current.is_empty() {
            token.clone()
        } else {
            format!("{current} · {token}")
        };
        if !current.is_empty() && Line::raw(&candidate).width() > width {
            lines.push(current);
            current = token;
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn render(frame: &mut Frame, app: &mut App) {
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
    let view_label = match view_mode {
        ViewMode::AlbumMetadata => "albums",
        ViewMode::Folders => "folder albums",
    };
    let summary = if app.search.is_some() || app.active_filter.is_some() {
        let (visible_albums, visible_tracks, visible_bytes) = app.visible_library_totals();
        format!(
            "{visible_albums}/{album_count} {view_label} · {visible_tracks}/{track_count} tracks · {}/{} · {} unreadable",
            format_bytes(visible_bytes),
            format_bytes(total_bytes),
            app.unreadable
        )
    } else {
        format!(
            "{album_count} {view_label} · {track_count} tracks · {} · {} unreadable",
            format_bytes(total_bytes),
            app.unreadable
        )
    };
    let content_width = frame.area().width.saturating_sub(2) as usize;
    let mut state_tokens = vec![match view_mode {
        ViewMode::AlbumMetadata => "VIEW: Albums".into(),
        ViewMode::Folders => "VIEW: Folders".into(),
    }];
    if let Some(playback) = &app.playback {
        let title_width = content_width.saturating_sub(10).min(36);
        state_tokens.push(format!(
            "PLAYING: {}",
            marquee(&playback.title, title_width, playback.started_at.elapsed())
        ));
    }
    if !app.conversion_queue.is_empty() {
        state_tokens.push(format!("QUEUE: {}", app.conversion_queue.len()));
    }
    if let Some(search) = &app.search {
        state_tokens.push(format!(
            "SEARCH: {}",
            truncate_to_width(search, content_width.saturating_sub(8).min(36))
        ));
    }
    if let Some(filter) = app.active_filter {
        state_tokens.push(format!("FILTER: {}", filter.label()));
    }
    let mut header_lines = vec![
        Line::raw(format!(
            "LIBRARY: {}",
            compact_header_path(&app.root, content_width.saturating_sub(9))
        )),
        Line::raw(truncate_to_width(&summary, content_width)),
    ];
    let state_style = if app.search.is_some() {
        let pulse_is_bright = (app.started_at.elapsed().as_millis() / 650).is_multiple_of(2);
        Style::default()
            .fg(Color::Black)
            .bg(if pulse_is_bright {
                Color::Yellow
            } else {
                Color::LightYellow
            })
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };
    header_lines.extend(
        wrap_header_tokens(state_tokens, content_width)
            .into_iter()
            .map(|line| Line::styled(line, state_style)),
    );
    let header_height = (header_lines.len() as u16 + 2).min(frame.area().height);
    let [header_area, table_area, footer_area] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    frame.render_widget(
        Paragraph::new(header_lines)
            .block(Block::default().borders(Borders::ALL).title(" Mausiker ")),
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
    let queued_paths = &app.conversion_queue;
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
            let is_queued = queued_paths.contains(&track.path);
            let queue_indicator = if is_queued { "[Q] " } else { "    " };
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
                    "{playing_indicator}└─ {queue_indicator}{}  {}",
                    track
                        .track_number
                        .map(|number| format!("{number:02}"))
                        .unwrap_or_else(|| "--".into()),
                    track.title
                ))
                .style(if is_queued {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }),
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
        "↑/k ↓/j select · Enter toggle · Space play/stop · e edit · i path · r rename · f filter · c queue · C convert · d delete · v view · Ctrl-K search · q quit"
            .into()
    };
    frame.render_widget(
        Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL)),
        footer_area,
    );

    if let Some(editor) = &app.editor {
        render_editor(frame, editor);
    }
    if let Some(paths) = &app.path_inspector {
        render_path_inspector(frame, paths);
    }
    if let Some(progress) = &app.conversion_progress {
        render_conversion_progress(frame, progress);
    }
    if let Some(preview) = &app.conversion_preview {
        render_conversion_preview(frame, preview);
    }
    if let Some(result) = &app.conversion_result {
        render_conversion_result(frame, result);
    }
    if let Some(preview) = &app.rename_preview {
        render_rename_confirmation(frame, preview);
    }
    if let Some(scroll) = app.delete_confirmation {
        render_delete_confirmation(frame, &app.completed_conversions, scroll);
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

fn render_rename_confirmation(frame: &mut Frame, preview: &RenamePreview) {
    let width = 110.min(frame.area().width.saturating_sub(4));
    let available_height = frame.area().height.saturating_sub(4);
    let visible_capacity = (available_height.saturating_sub(6) as usize / 2).max(1);
    let start = preview
        .scroll
        .min(preview.plans.len().saturating_sub(visible_capacity));
    let visible_plans = preview
        .plans
        .len()
        .saturating_sub(start)
        .min(visible_capacity);
    let height = (visible_plans as u16 * 2 + 6).min(available_height).max(5);
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::raw(format!(
        "{} track(s) ready · {} skipped · showing {}–{} of {}",
        preview.ready_count(),
        preview.skipped_count(),
        start + 1,
        start + visible_plans,
        preview.plans.len()
    ))];
    for plan in preview.plans.iter().skip(start).take(visible_plans) {
        match (&plan.status, &plan.target) {
            (RenameStatus::Ready, Some(target)) => {
                lines.push(Line::raw(format!(
                    "RENAME  {}",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(format!(
                    "    →   {}",
                    compact_preview_path(target)
                )));
            }
            (RenameStatus::Unchanged, _) => {
                lines.push(Line::raw(format!(
                    "KEEP    {} (already matches)",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(""));
            }
            (RenameStatus::MissingTrackNumber, _) => {
                lines.push(Line::raw(format!(
                    "SKIP    {} (missing track number)",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(""));
            }
            (RenameStatus::Conflict, Some(target)) => {
                lines.push(Line::raw(format!(
                    "SKIP    {}",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(format!(
                    "    →   {} (target already exists or is duplicated)",
                    compact_preview_path(target)
                )));
            }
            _ => unreachable!("rename plans with a target status include a target path"),
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "↑/k ↓/j review · y confirm rename · Esc keep current names",
    ));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Review rename "),
        ),
        popup,
    );
}

fn render_delete_confirmation(
    frame: &mut Frame,
    conversions: &[CompletedConversion],
    scroll: usize,
) {
    let width = 110.min(frame.area().width.saturating_sub(4));
    let available_height = frame.area().height.saturating_sub(4);
    let visible_capacity = (available_height.saturating_sub(7) as usize / 2).max(1);
    let start = scroll.min(conversions.len().saturating_sub(visible_capacity));
    let visible_conversions = conversions
        .len()
        .saturating_sub(start)
        .min(visible_capacity);
    let height = (visible_conversions as u16 * 2 + 7)
        .min(available_height)
        .max(5);
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::raw(format!(
        "Delete {} verified original track(s)? This cannot be undone. Showing {}–{} of {}.",
        conversions.len(),
        start + 1,
        start + visible_conversions,
        conversions.len()
    ))];
    for completed in conversions.iter().skip(start).take(visible_conversions) {
        lines.push(Line::raw(format!(
            "DELETE  {}",
            completed.original.display()
        )));
        lines.push(Line::raw(format!("KEEP    {}", completed.output.display())));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "↑/k ↓/j review · M4As are re-verified.  y confirm deletion · Esc keep originals",
    ));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Review original-file deletion "),
        ),
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
        Line::raw("←/→ move cursor · Tab/↑/↓ switch field · Enter save · Esc cancel"),
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
    let active_label = editor.labels()[editor.active_field];
    let active_value = &editor.values[editor.active_field];
    let cursor_prefix = format!("> {active_label}: {}", &active_value[..editor.cursor()]);
    let cursor_column = Line::raw(cursor_prefix).width() as u16;
    let cursor_x = popup.x + 1 + cursor_column;
    let cursor_y = popup.y + 3 + editor.active_field as u16;
    if cursor_x < popup.x + popup.width - 1 && cursor_y < popup.y + popup.height - 1 {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn renamed_track_path(track: &Track) -> Result<PathBuf, ()> {
    let number = track.track_number.ok_or(())?;
    let title = sanitized_file_stem(&track.title);
    let extension = track
        .path
        .extension()
        .and_then(|extension| extension.to_str());
    let filename = match extension {
        Some(extension) if !extension.is_empty() => format!("{number:02}_{title}.{extension}"),
        _ => format!("{number:02}_{title}"),
    };
    Ok(track.path.with_file_name(filename))
}

fn build_rename_preview(tracks: Vec<Track>) -> RenamePreview {
    let proposed_targets: BTreeMap<_, _> = tracks
        .iter()
        .filter_map(|track| {
            renamed_track_path(track)
                .ok()
                .filter(|target| target != &track.path)
        })
        .fold(BTreeMap::new(), |mut targets, target| {
            *targets.entry(target).or_insert(0usize) += 1;
            targets
        });
    let plans = tracks
        .into_iter()
        .map(|track| match renamed_track_path(&track) {
            Err(()) => RenamePlan {
                source: track.path,
                target: None,
                status: RenameStatus::MissingTrackNumber,
            },
            Ok(target) if target == track.path => RenamePlan {
                source: track.path,
                target: Some(target),
                status: RenameStatus::Unchanged,
            },
            Ok(target)
                if target.exists()
                    || proposed_targets
                        .get(&target)
                        .is_some_and(|count| *count > 1) =>
            {
                RenamePlan {
                    source: track.path,
                    target: Some(target),
                    status: RenameStatus::Conflict,
                }
            }
            Ok(target) => RenamePlan {
                source: track.path,
                target: Some(target),
                status: RenameStatus::Ready,
            },
        })
        .collect();
    RenamePreview { plans, scroll: 0 }
}

fn build_conversion_preview(root: &Path, queued: &[PathBuf]) -> ConversionPreview {
    let plans = queued
        .iter()
        .map(|source| {
            if source
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m4a"))
            {
                return ConversionPlan {
                    source: source.clone(),
                    output: Some(source.clone()),
                    status: ConversionPlanStatus::AlreadyM4a,
                };
            }
            match conversion::output_path(root, source) {
                Err(_) => ConversionPlan {
                    source: source.clone(),
                    output: None,
                    status: ConversionPlanStatus::OutsideLibrary,
                },
                Ok(output) if output.exists() => ConversionPlan {
                    source: source.clone(),
                    output: Some(output),
                    status: ConversionPlanStatus::OutputExists,
                },
                Ok(output) => ConversionPlan {
                    source: source.clone(),
                    output: Some(output),
                    status: ConversionPlanStatus::Ready,
                },
            }
        })
        .collect();
    ConversionPreview { plans, scroll: 0 }
}

fn compact_preview_path(path: &Path) -> String {
    let components: Vec<_> = path.components().collect();
    if components.len() <= 5 {
        return path.display().to_string();
    }

    let mut prefix = PathBuf::new();
    for component in components.iter().take(3) {
        prefix.push(component.as_os_str());
    }
    prefix.push("…");
    prefix.push(components[components.len() - 2].as_os_str());
    prefix.push(
        components
            .last()
            .expect("a non-empty path has a final component")
            .as_os_str(),
    );
    prefix.display().to_string()
}

fn sanitized_file_stem(title: &str) -> String {
    let mut filename = String::new();
    for character in title.trim().trim_matches('.').chars() {
        let separator = matches!(
            character,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
        ) || character.is_whitespace();
        if separator {
            if !filename.ends_with('_') {
                filename.push('_');
            }
        } else {
            filename.push(character);
        }
    }
    if filename.is_empty() {
        "Untitled".into()
    } else {
        filename
    }
}

fn render_path_inspector(frame: &mut Frame, paths: &[PathBuf]) {
    let width = 100.min(frame.area().width.saturating_sub(4));
    let requested_height = paths.len() as u16 + 4;
    let height = requested_height
        .min(frame.area().height.saturating_sub(4))
        .max(5);
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let visible_paths = if requested_height > height {
        paths.len().min(height.saturating_sub(5) as usize)
    } else {
        paths.len()
    };
    let mut lines: Vec<_> = paths
        .iter()
        .take(visible_paths)
        .map(|path| Line::raw(path.display().to_string()))
        .collect();
    if visible_paths < paths.len() {
        lines.push(Line::raw(format!(
            "… and {} more",
            paths.len() - visible_paths
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw("Esc, Enter, or i to close"));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" File paths ")),
        popup,
    );
}

fn render_conversion_progress(frame: &mut Frame, progress: &ConversionProgress) {
    let width = 48.min(frame.area().width.saturating_sub(4));
    let height = 9.min(frame.area().height.saturating_sub(4));
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let remaining = progress.total.saturating_sub(progress.processed);
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spinner =
        SPINNER[(progress.started_at.elapsed().as_millis() / 80 % SPINNER.len() as u128) as usize];
    let current = progress
        .current_path
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("Preparing next track…");
    let lines = vec![
        Line::styled(
            format!("{spinner} Converting: {current}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw(format!(
            "Processed: {}/{}",
            progress.processed, progress.total
        )),
        Line::styled(
            format!("Successful: {}", progress.successful),
            Style::default().fg(Color::Green),
        ),
        Line::styled(
            format!("Unsuccessful: {}", progress.unsuccessful),
            Style::default().fg(Color::Red),
        ),
        Line::raw(format!("Remaining: {remaining}")),
    ];
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Conversion progress "),
        ),
        popup,
    );
}

fn render_conversion_preview(frame: &mut Frame, preview: &ConversionPreview) {
    let width = 110.min(frame.area().width.saturating_sub(4));
    let available_height = frame.area().height.saturating_sub(4);
    let visible_capacity = (available_height.saturating_sub(6) as usize / 2).max(1);
    let start = preview
        .scroll
        .min(preview.plans.len().saturating_sub(visible_capacity));
    let visible_plans = preview
        .plans
        .len()
        .saturating_sub(start)
        .min(visible_capacity);
    let height = (visible_plans as u16 * 2 + 6).min(available_height).max(5);
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::raw(format!(
        "M4A files are written beside their sources · {} ready · {} skipped · showing {}–{} of {}",
        preview.ready_count(),
        preview.skipped_count(),
        start + 1,
        start + visible_plans,
        preview.plans.len()
    ))];
    for plan in preview.plans.iter().skip(start).take(visible_plans) {
        match (&plan.status, &plan.output) {
            (ConversionPlanStatus::Ready, Some(output)) => {
                lines.push(Line::raw(format!(
                    "CONVERT  {}",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(format!(
                    "    →    {}",
                    compact_preview_path(output)
                )));
            }
            (ConversionPlanStatus::AlreadyM4a, _) => {
                lines.push(Line::raw(format!(
                    "SKIP     {} (already M4A)",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(""));
            }
            (ConversionPlanStatus::OutputExists, Some(output)) => {
                lines.push(Line::raw(format!(
                    "SKIP     {}",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(format!(
                    "    →    {} (output already exists)",
                    compact_preview_path(output)
                )));
            }
            (ConversionPlanStatus::OutsideLibrary, _) => {
                lines.push(Line::raw(format!(
                    "SKIP     {} (outside the selected library)",
                    compact_preview_path(&plan.source)
                )));
                lines.push(Line::raw(""));
            }
            _ => unreachable!("conversion plans have an output whenever their status needs one"),
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        "↑/k ↓/j review · y start ready conversions · Esc keep queue",
    ));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Review M4A conversion "),
        ),
        popup,
    );
}

fn render_conversion_result(frame: &mut Frame, result: &ConversionResult) {
    let width = 110.min(frame.area().width.saturating_sub(4));
    let available_height = frame.area().height.saturating_sub(4);
    let visible_capacity = (available_height.saturating_sub(6) as usize / 2).max(1);
    let start = result
        .scroll
        .min(result.total().saturating_sub(visible_capacity));
    let visible_entries = result.total().saturating_sub(start).min(visible_capacity);
    let height = (visible_entries as u16 * 2 + 6)
        .min(available_height)
        .max(5);
    let popup = ratatui::layout::Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };
    let mut lines = vec![Line::raw(format!(
        "{} verified · {} failed · {} remain queued · showing {}–{} of {}",
        result.successful.len(),
        result.failures.len(),
        result.remaining_queued,
        start + 1,
        start + visible_entries,
        result.total()
    ))];
    for entry in start..start + visible_entries {
        if let Some(completed) = result.successful.get(entry) {
            lines.push(Line::raw(format!(
                "VERIFIED  {}",
                compact_preview_path(&completed.original)
            )));
            lines.push(Line::raw(format!(
                "    →     {}",
                compact_preview_path(&completed.output)
            )));
        } else {
            let failure = &result.failures[entry - result.successful.len()];
            lines.push(Line::raw(format!(
                "FAILED    {}",
                compact_preview_path(&failure.source)
            )));
            lines.push(Line::raw(format!("    {}", failure.error)));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw("↑/k ↓/j review · Esc close"));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Conversion results "),
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
    fn groups_compilation_tracks_with_different_album_artists() {
        let albums = group_by_album(vec![
            track("First", "Artist One", "Artist One", "Smash Hits Vol. 20", 1),
            track(
                "Second",
                "Artist Two",
                "Artist Two",
                "Smash Hits Vol. 20",
                2,
            ),
        ]);

        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].artist, "Various Artists");
        assert_eq!(albums[0].tracks.len(), 2);
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

    #[test]
    fn visible_library_totals_reflect_active_search_and_track_filter() {
        let mut first = track("First", "Artist One", "Artist One", "Record One", 1);
        first.release_date = None;
        let app = App::from_tracks(
            PathBuf::new(),
            vec![
                first,
                track("Second", "Artist One", "Artist One", "Record One", 2),
                track("Third", "Artist Two", "Artist Two", "Record Two", 1),
            ],
            0,
        );
        let mut app = app;
        app.search = Some("record one".into());

        assert_eq!(app.visible_library_totals(), (1, 2, 2));

        app.active_filter = Some(TrackFilter::NoReleaseDate);
        assert_eq!(app.visible_library_totals(), (1, 1, 1));
    }

    #[test]
    fn metadata_editor_separates_album_artist_from_track_artist() {
        let album_editor = MetadataEditor {
            target: EditorTarget::Album(0),
            values: Vec::new(),
            active_field: 0,
            cursor_positions: Vec::new(),
        };
        let track_editor = MetadataEditor {
            target: EditorTarget::Track { album: 0, track: 0 },
            values: Vec::new(),
            active_field: 0,
            cursor_positions: Vec::new(),
        };
        assert_eq!(
            album_editor.labels(),
            ["Album artist", "Album", "Release date"]
        );
        assert_eq!(
            track_editor.labels(),
            ["Title", "Artist", "Album", "Release date"]
        );
    }

    #[test]
    fn metadata_editor_edits_at_a_unicode_aware_cursor() {
        let mut editor = MetadataEditor {
            target: EditorTarget::Album(0),
            values: vec!["Beyoncé".into()],
            active_field: 0,
            cursor_positions: vec!["Beyoncé".len()],
        };

        editor.move_cursor_left();
        editor.move_cursor_left();
        editor.insert('!');
        editor.backspace();
        editor.delete();

        assert_eq!(editor.values[0], "Beyoné");
        assert_eq!(editor.cursor(), "Beyon".len());
    }

    #[test]
    fn renamed_track_path_uses_number_title_and_original_extension() {
        let mut track = track("A / B? C", "Artist", "Artist", "Album", 3);
        track.path = PathBuf::from("/music/old-name.FLAC");

        assert_eq!(
            renamed_track_path(&track).unwrap(),
            PathBuf::from("/music/03_A_B_C.FLAC")
        );
    }

    #[test]
    fn renamed_track_path_requires_a_track_number() {
        let mut track = track("Title", "Artist", "Artist", "Album", 1);
        track.track_number = None;

        assert!(renamed_track_path(&track).is_err());
    }

    #[test]
    fn rename_preview_identifies_unchanged_missing_and_conflicting_tracks() {
        let mut unchanged = track("First", "Artist", "Artist", "Album", 1);
        unchanged.path = PathBuf::from("/not-a-real-library/01_First.mp3");
        let mut missing_number = track("Second", "Artist", "Artist", "Album", 2);
        missing_number.path = PathBuf::from("/not-a-real-library/old.mp3");
        missing_number.track_number = None;
        let mut duplicate_first = track("Duplicate", "Artist", "Artist", "Album", 3);
        duplicate_first.path = PathBuf::from("/not-a-real-library/a.mp3");
        let mut duplicate_second = track("Duplicate", "Artist", "Artist", "Album", 3);
        duplicate_second.path = PathBuf::from("/not-a-real-library/b.mp3");
        let mut ready = track("Ready", "Artist", "Artist", "Album", 4);
        ready.path = PathBuf::from("/not-a-real-library/old-ready.mp3");

        let preview = build_rename_preview(vec![
            unchanged,
            missing_number,
            duplicate_first,
            duplicate_second,
            ready,
        ]);

        assert_eq!(preview.ready_count(), 1);
        assert_eq!(preview.skipped_count(), 4);
        assert_eq!(preview.plans[0].status, RenameStatus::Unchanged);
        assert_eq!(preview.plans[1].status, RenameStatus::MissingTrackNumber);
        assert_eq!(preview.plans[2].status, RenameStatus::Conflict);
        assert_eq!(preview.plans[3].status, RenameStatus::Conflict);
        assert_eq!(preview.plans[4].status, RenameStatus::Ready);
    }

    #[test]
    fn compact_preview_path_keeps_the_filename_and_meaningful_context() {
        let long_path = Path::new("/home/glenn/Downloads/Illmatic/1_Nas_Illmatic_TheGenesis.m4a");
        let mut expected = PathBuf::new();
        for component in long_path.components().take(3) {
            expected.push(component.as_os_str());
        }
        expected.push("…");
        expected.push("Illmatic");
        expected.push("1_Nas_Illmatic_TheGenesis.m4a");
        assert_eq!(
            compact_preview_path(long_path),
            expected.display().to_string()
        );
        let short_path = Path::new("/music/Illmatic/01_The_Genesis.m4a");
        assert_eq!(
            compact_preview_path(short_path),
            short_path.display().to_string()
        );
    }

    #[test]
    fn responsive_header_helpers_keep_state_readable_within_the_available_width() {
        let path = compact_header_path(
            Path::new("/home/glenn/Downloads/Illmatic/1_Nas_Illmatic_TheGenesis.m4a"),
            24,
        );
        assert!(path.starts_with("…/1_Nas_Illmatic"));
        assert_eq!(Line::raw(&path).width(), 24);

        let lines = wrap_header_tokens(
            vec![
                "VIEW: Albums".into(),
                "PLAYING: The Genesis".into(),
                "QUEUE: 12".into(),
                "FILTER: No release date".into(),
            ],
            24,
        );
        assert!(lines.iter().all(|line| Line::raw(line).width() <= 24));
        assert!(lines.iter().any(|line| line.contains("PLAYING")));
        assert!(lines.iter().any(|line| line.contains("QUEUE")));
    }

    #[test]
    fn conversion_preview_shows_outputs_and_skips_unconvertible_sources() {
        let preview = build_conversion_preview(
            Path::new("/not-a-real-library"),
            &[
                PathBuf::from("/not-a-real-library/song.flac"),
                PathBuf::from("/not-a-real-library/already.m4a"),
                PathBuf::from("/outside-library/song.mp3"),
            ],
        );

        assert_eq!(preview.ready_count(), 1);
        assert_eq!(preview.skipped_count(), 2);
        assert_eq!(preview.plans[0].status, ConversionPlanStatus::Ready);
        assert_eq!(
            preview.plans[0].output.as_deref(),
            Some(Path::new("/not-a-real-library/song.m4a"))
        );
        assert_eq!(preview.plans[1].status, ConversionPlanStatus::AlreadyM4a);
        assert_eq!(
            preview.plans[2].status,
            ConversionPlanStatus::OutsideLibrary
        );
    }

    #[test]
    fn conversion_progress_counts_successes_and_failures() {
        let mut app = App::from_tracks(PathBuf::new(), Vec::new(), 0);
        let (sender, receiver) = mpsc::channel();
        app.conversion_progress = Some(ConversionProgress {
            total: 2,
            processed: 0,
            successful: 0,
            unsuccessful: 0,
            current_path: None,
            started_at: Instant::now(),
        });
        app.conversion_receiver = Some(receiver);
        sender
            .send(ConversionUpdate::Started(PathBuf::from("current.mp3")))
            .unwrap();
        app.refresh_conversion();
        assert_eq!(
            app.conversion_progress
                .as_ref()
                .unwrap()
                .current_path
                .as_deref(),
            Some(Path::new("current.mp3"))
        );
        sender
            .send(ConversionUpdate::Finished {
                source: PathBuf::from("failed.mp3"),
                result: Err("conversion failed".into()),
            })
            .unwrap();

        app.refresh_conversion();

        let progress = app.conversion_progress.as_ref().unwrap();
        assert_eq!(progress.processed, 1);
        assert_eq!(progress.successful, 0);
        assert_eq!(progress.unsuccessful, 1);
        assert_eq!(app.conversion_queue, [PathBuf::from("failed.mp3")]);
    }

    #[test]
    fn queue_key_toggles_a_track_and_advances_to_the_next_row() {
        let mut app = App::from_tracks(
            PathBuf::new(),
            vec![
                track("First", "Artist", "Artist", "Album", 1),
                track("Second", "Artist", "Artist", "Album", 2),
            ],
            0,
        );
        app.expanded_albums.insert(0);
        app.state.select(Some(1));

        app.enqueue_selected();

        assert_eq!(app.conversion_queue, [PathBuf::from("1.mp3")]);
        assert_eq!(
            app.selected_row(),
            Some(LibraryRow::Track { album: 0, track: 1 })
        );

        app.state.select(Some(1));
        app.enqueue_selected();

        assert!(app.conversion_queue.is_empty());
        assert_eq!(
            app.selected_row(),
            Some(LibraryRow::Track { album: 0, track: 1 })
        );
    }

    #[test]
    fn queue_key_on_an_album_toggles_all_eligible_tracks() {
        let mut app = App::from_tracks(
            PathBuf::new(),
            vec![
                track("First", "Artist", "Artist", "Album", 1),
                track("Second", "Artist", "Artist", "Album", 2),
            ],
            0,
        );

        app.enqueue_selected();

        assert_eq!(
            app.conversion_queue,
            [PathBuf::from("1.mp3"), PathBuf::from("2.mp3")]
        );

        app.enqueue_selected();

        assert!(app.conversion_queue.is_empty());
    }

    #[test]
    fn applying_a_filter_keeps_expanded_items_open() {
        let mut app = App::from_tracks(
            PathBuf::new(),
            vec![track("Title", "Artist", "Artist", "Album", 1)],
            0,
        );
        app.expanded_albums.insert(0);
        app.expanded_folder_groups.insert(0);
        app.filter_menu = Some(1);

        app.handle_filter_menu_key(KeyCode::Enter);

        assert!(app.expanded_albums.contains(&0));
        assert!(app.expanded_folder_groups.contains(&0));
    }
}
