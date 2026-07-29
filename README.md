# mausiker

`mausiker` is a terminal music-library manager built with [Ratatui].

## Current capabilities

- Recursively scan common audio files and read their metadata without changing them.
- Browse a library grouped by album and primary artist; expand albums to see their tracks.
- Show track numbers, album duration, file size, format, and release year.
- Search album titles, primary artists, and featured track artists.
- Edit metadata from the terminal:
  - Album edits update the album title and release date across all tracks in that album.
  - Track edits update the title, album title, and release date for that track only.
  - Release dates are validated as `YYYY`, `YYYY-MM`, or `YYYY-MM-DD` before saving.
- Play a selected track with an animated playback indicator and scrolling now-playing title.

## Controls

| Key | Action |
| --- | --- |
| `j` / `↓`, `k` / `↑` | Move selection |
| `Enter` | Expand or collapse the selected album |
| `→` / `l`, `←` / `h` | Expand or collapse an album |
| `Space` | Play or stop the selected track |
| `e` | Edit selected album or track metadata |
| `Ctrl-K` | Search albums and artists |
| `Esc` | Cancel an edit/search, clear an active filter, or quit |
| `r` | Rescan the library |
| `q` | Quit |

## Playback requirement

Playback uses `ffplay`, which is distributed with FFmpeg. Install FFmpeg and make sure `ffplay` is available on your `PATH`. This works on Linux, macOS, or Windows when `ffplay` is installed.

## Run

Scan the current directory:

```sh
cargo run
```

Or scan a specific music directory:

```sh
cargo run -- /path/to/music
```

Conversion is intentionally not enabled yet: the next feature will build an explicit conversion queue with a separate output folder and post-conversion verification before any deletion can be offered.

[Ratatui]: https://ratatui.rs

## License

Copyright (c) glennDittmann <glenn.dittmann@posteo.de>

This project is licensed under the MIT license ([LICENSE] or <http://opensource.org/licenses/MIT>)

[LICENSE]: ./LICENSE
