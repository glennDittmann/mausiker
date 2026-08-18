# Mausiker plan

- [x] Read-only music-library scanner and terminal browser
- [x] Organize code by library and browser features, with focused unit tests
- [x] Edit track and album title/date metadata from the library browser
- [x] Edit album artists without changing individual track artist credits
- [x] Validate release-date input before saving metadata
- [ ] UI refinement
  - [x] Group music by album, with expandable albums and drill-in navigation
  - [x] Group featured-artist tracks under the primary album artist
  - [x] Show the total track duration on each album row
  - [x] Sort tracks by track number and show zero-padded prefixes
  - [x] Collapse an album from its header or any selected track
  - [x] Search albums and artists with Ctrl-K, with a reversible filter
  - [x] Make active search filters prominent with a subtle visual pulse
  - [x] Include featured track artists in album search results
  - [x] Filter tracks by release-date completeness or recent file changes
- [x] Safe conversion queue: side-by-side output, metadata preservation, verification, and explicit original-file deletion
- [x] Preview the selected song with Space
- [x] Configure subfolders to exclude from library scans
- [x] Add a folder view: treat a track's parent as its album folder, that folder's parent as its artist, and any higher folder as a music grouping
- [x] Mark tracks selected for queue in UI
- [x] Automatic renaming of tracks/albums to `NN_song_name`, using the track number and title
- [x] Progress popup for music conversion

## Next UX work (highest user impact first)

- [ ] Make search and filters show result counts against library totals, reveal relevant matches, and highlight the field that matched.
- [x] Add a review-and-confirm step for renames and original-file deletion, including affected paths, proposed names, and skipped conflicts.
- [x] Add a conversion preflight (queued items, output location, conflicts) and an inspectable per-file result summary when it finishes.
- [ ] Make `c` toggle queued tracks and advance; on an album or folder, toggle all eligible tracks instead of only adding them.
- [ ] Make the header responsive so path, playback, queue, and filter state remain readable in narrow terminals.
- [ ] Clarify table semantics: use a `Tracks` column for album counts and remove or repurpose redundant album-format cells.
- [ ] Add a `?` help overlay and keep it as the single source of truth for in-app controls and README documentation.
- [ ] Apply a terminal-theme-resilient visual system with explicit state labels/markers, sufficient selection contrast, and color as secondary meaning.
- [ ] Make long path/result dialogs navigable, and make blocked metadata saves immediately explain which field needs attention.
