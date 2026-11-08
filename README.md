# Margin

A text editor for files of any size, for Linux and Windows. Open a 300 GB log as fast as a
three-line note: nothing is loaded up front, scrolling and jumping work immediately, and editing in
the middle of a huge file costs the same as anywhere else.

## How it handles huge files

- **Nothing is read up front.** Big files are memory-mapped; the editor only ever reads the rows on
  screen.
- **Edits don't copy the file.** The document is a piece table over the file: typing, pasting,
  replace-all and undo only rearrange pieces. Undo of a replace-all over 100 GB is instant.
- **Line numbers arrive in the background.** Lines are counted on a separate thread (a 20 GB file
  in about a second when it's cached, at disk speed otherwise). Until then you can scroll, jump to a
  percentage and search; line numbers fill in when the count gets there. The index takes 8 bytes per
  64 KB, so 300 GB needs about 37 MB of memory.
- **Lines with no end.** A 40 GB file with no line breaks shows as rows of a few KB, so it scrolls
  like any other file.
- **Searching streams.** Find, count, find-all and replace-all run in windows of a few MB on a
  thread, with progress and a Stop button.
- **Saving is safe.** Files are written beside the original and renamed over it, so a failed or
  cancelled save never leaves half a file.

Measured on a 20 GB, 198-million-line log: open 54 µs, jump to the end 26 µs, go to line
66,006,602 in 11 µs, edit in the middle in 5 µs, search 10 GB in 0.9 s, save in 12 s.

## What it does

| | |
|---|---|
| **Editing** | Tabs, undo/redo, auto-indent, soft tabs, overwrite mode, duplicate / delete / move / join lines, toggle comment, UPPER/lower case, sort, remove duplicates, trim trailing whitespace, tabs ↔ spaces |
| **Find** | Incremental find with case, whole-word and regex; replace with `$1` groups; replace in selection; find all into a results list; find in files (respects `.gitignore`, file filters) |
| **Navigation** | Go to line or percentage, bookmarks (`Ctrl+F2`, `F2`), matching brackets, other uses of the selected word highlighted |
| **Languages** | Syntax highlighting for about 200 languages, plus a highlighter for log files (timestamps, levels, key=value, IPs) |
| **Files** | UTF-8 (with or without BOM), UTF-16 LE/BE and legacy code pages (detected), LF/CRLF/CR, follow a growing file (`tail -f`), reload when changed on disk, save as administrator (Linux, via pkexec) |
| **Sessions** | Open files, caret positions and unsaved work come back next time; closing never asks about anything it could back up |
| **One window** | Opening files from a file manager or terminal adds tabs to the window that's already open (`margin -n` for a separate one); `margin file:120` opens at line 120 |

Keyboard: `Ctrl+F` find · `Ctrl+H` replace · `F3`/`Shift+F3` next/previous · `Alt+Enter` find all ·
`Ctrl+Shift+F` find in files · `Ctrl+G` go to line · `Ctrl+D` duplicate · `Ctrl+L` delete line ·
`Ctrl+Shift+↑/↓` move lines · `Ctrl+Q` comment · `Ctrl+J` join · `Alt+Z` wrap ·
`Ctrl+Tab` next tab · `Ctrl+Shift+T` reopen closed tab · `Ctrl+±/0` text size.

## Layout

- `crates/margin-core`: the engine (piece table, line index, rows, search, files), UI-independent and
  tested on its own. `cargo run --release -p margin-core --example stress -- <file>` times it on a
  file of your choice.
- `src`: the interface (egui).

## Building

Rust 1.88 or newer. On Linux:

```sh
scripts/install-local.sh            # build and install for your user (~/.local), or
PREFIX=/usr/local sudo -E scripts/install-local.sh --no-build   # after `cargo build --release`
```

On Windows: `cargo build --release`, then run `target\release\margin.exe`.
