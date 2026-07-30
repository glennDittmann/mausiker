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
  - [ ] Define and apply a cohesive color scheme
- [x] Safe conversion queue: separate output folder, metadata preservation, verification, and explicit original-file deletion

## Future ideas

- [x] Preview the selected song with Space
- [x] Configure subfolders to exclude from library scans
- [x] Add a folder view: treat a track's parent as its album folder, that folder's parent as its artist, and any higher folder as a music grouping
