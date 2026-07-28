# mausiker

`mausiker` is a terminal music-library manager built with [Ratatui].

## Current capabilities

- Safely scan a directory recursively (no files are changed).
- Read title, artist, album, duration, format, and size from common audio files.
- Browse the scanned library in the terminal.

## Run

Scan the current directory:

```sh
cargo run
```

Or scan a specific music directory:

```sh
cargo run -- /path/to/music
```

Use `j`/`↓` and `k`/`↑` to select a track, `r` to rescan, and `q` or `Esc` to quit.

Conversion is intentionally not enabled yet: the next feature will build an explicit conversion queue with a separate output folder and post-conversion verification before any deletion can be offered.

[Ratatui]: https://ratatui.rs
[Hello World Template]: https://github.com/ratatui/templates/tree/main/hello-world

## License

Copyright (c) glennDittmann <glenn.dittmann@posteo.de>

This project is licensed under the MIT license ([LICENSE] or <http://opensource.org/licenses/MIT>)

[LICENSE]: ./LICENSE
