# Mausiker plan

- [x] Read-only music-library scanner and terminal browser
- [x] Organize code by library and browser features, with focused unit tests
- [x] Edit track and album title/date metadata from the library browser
- [x] Edit album artists without changing individual track artist credits
- [x] Validate release-date input before saving metadata
- [x] Read and edit genre metadata, with a `No genre` track filter
- [x] Read, validate, and edit disc number, disc total, and disc subtitle metadata
- [x] Show multi-disc albums as an album → disc → track hierarchy, with disc-level actions and editing
- [x] UI refinement
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

- [x] Make search and filters show result counts against library totals, reveal relevant matches, and highlight the field that matched.
- [x] Add a review-and-confirm step for renames and original-file deletion, including affected paths, proposed names, and skipped conflicts.
- [x] Add a conversion preflight (queued items, output location, conflicts) and an inspectable per-file result summary when it finishes.
- [x] Make `c` toggle queued tracks and advance; on an album or folder, toggle all eligible tracks instead of only adding them.
- [x] Make the header responsive so path, playback, queue, and filter state remain readable in narrow terminals.
- [x] Clarify table semantics: use a `Tracks` column for album counts and remove or repurpose redundant album-format cells.
- [x] Add a `?` help overlay and keep it as the single source of truth for in-app controls and README documentation.
- [x] Apply a terminal-theme-resilient visual system with explicit state labels/markers, sufficient selection contrast, and color as secondary meaning.
- [x] Make long path/result dialogs navigable, and make blocked metadata saves immediately explain which field needs attention.

## MusicBrainz comparison

- [x] Compare selected album metadata with the best read-only MusicBrainz release match.

## Album artwork embedding

- [ ] Detect whether each track already has embedded front-cover artwork and preserve it by default.
- [ ] Resolve missing album artwork in a deterministic order: another track from the same album, then a local `cover`/`folder`/`front` JPEG or PNG, then an optional confirmed Cover Art Archive result.
- [ ] Match albums using album artist, album title, and directory so similarly named releases cannot accidentally share artwork.
- [ ] Show an album-level preview of the selected artwork, its source, affected tracks, and any skipped or ambiguous tracks before writing.
- [ ] Embed one verified JPEG or PNG into every M4A track as standard `covr` metadata without re-encoding audio; reject unsupported or excessively large images.
- [ ] Write changes to a same-directory partial file, reopen it to verify artwork and audio properties, and only then atomically publish it; never replace existing artwork without explicit confirmation.
- [ ] Integrate artwork embedding into conversion before the existing output verification and final rename, and use the same safe temporary-copy workflow for existing M4A files.
