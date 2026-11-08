//! A document being edited: text, selection, undo history, and the editing commands built on them.

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::file::{Encoding, Eol};
use crate::rows;
use crate::text::{Piece, Source, Text};

/// Line operations and similar work on at most this much text at once.
pub const OP_MAX: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sel {
    /// where the selection started
    pub anchor: u64,
    /// where the caret is
    pub head: u64,
}

impl Sel {
    pub fn caret(at: u64) -> Sel {
        Sel { anchor: at, head: at }
    }
    pub fn range(&self) -> Range<u64> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Debug)]
enum Change {
    /// at `at`, `removed` pieces were replaced by `inserted` pieces
    Splice { at: u64, removed: Vec<Piece>, inserted: Vec<Piece> },
    /// the whole piece list was swapped (replace all, line operations on a lot of text)
    Swap { before: Vec<Piece>, after: Vec<Piece> },
}

#[derive(Clone, Debug)]
struct Step {
    /// identifies the text state this step leads to
    id: u64,
    changes: Vec<Change>,
    before: Sel,
    after: Sel,
    kind: Kind,
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Typing,
    Deleting,
    Other,
}

pub struct Document {
    pub text: Text,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    pub eol: Eol,
    pub sel: Sel,
    /// bookmarked offsets (start of a row)
    pub bookmarks: Vec<u64>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// bumped on every change; the UI keys caches on it
    pub version: u64,
    /// the text state on disk (see `state`)
    saved_state: u64,
    next_step: u64,
    /// the steps since the last save are only undoable down to here (after a reload)
    pub binary: bool,
}

impl Document {
    pub fn new(text: Text, path: Option<PathBuf>, encoding: Encoding, eol: Eol) -> Document {
        Document {
            text,
            path,
            encoding,
            eol,
            sel: Sel::default(),
            bookmarks: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            version: 1,
            saved_state: 0,
            next_step: 1,
            binary: false,
        }
    }

    pub fn empty() -> Document {
        Document::new(Text::default(), None, Encoding::Utf8, Eol::native())
    }

    /// Which text state this is: undoing back to a state gives the same id again, so undoing
    /// every change since a save makes the document clean.
    pub fn state(&self) -> u64 {
        self.undo.last().map(|s| s.id).unwrap_or(0)
    }

    pub fn dirty(&self) -> bool {
        self.state() != self.saved_state
    }

    pub fn mark_saved(&mut self) {
        self.saved_state = self.state();
    }

    /// The text as of `state` is what's on disk (a save that finished while editing went on).
    pub fn mark_saved_at(&mut self, state: u64) {
        self.saved_state = state;
    }

    /// Forgets that it was ever saved (a backup restored after closing, say).
    pub fn mark_unsaved(&mut self) {
        self.saved_state = u64::MAX;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ------------------------------------------------------------ the one way text changes

    /// Replaces each range with its bytes, in one undo step. Ranges must not overlap; any order.
    /// The selection afterwards is `after(map)`, where `map` translates old offsets to new ones.
    pub fn edit(&mut self, mut edits: Vec<(Range<u64>, Vec<u8>)>, kind: Kind, after: impl FnOnce(&dyn Fn(u64) -> u64) -> Sel) {
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|e| e.0.start);
        let before = self.sel;
        // apply from the end so earlier offsets stay valid
        let mut changes = Vec::with_capacity(edits.len());
        for (r, bytes) in edits.iter().rev() {
            let inserted = self.text.store(bytes);
            let removed = self.text.splice(r.start, r.end - r.start, &inserted);
            changes.push(Change::Splice { at: r.start, removed, inserted });
        }
        let shifts: Vec<(u64, u64, i64)> = edits.iter().map(|(r, b)| (r.start, r.end, b.len() as i64 - (r.end - r.start) as i64)).collect();
        let map = move |x: u64| -> u64 {
            let mut d: i64 = 0;
            for &(s, e, delta) in &shifts {
                if x >= e {
                    d += delta;
                } else if x > s {
                    // inside a replaced range: clamp to the end of its replacement
                    return (s as i64 + d + (e - s) as i64 + delta).max(s as i64 + d) as u64;
                } else {
                    break;
                }
            }
            (x as i64 + d) as u64
        };
        let sel = after(&map);
        self.bookmarks = self.bookmarks.iter().map(|&b| map(b)).collect();
        self.bookmarks.dedup();
        self.commit(changes, before, sel, kind);
    }

    fn commit(&mut self, changes: Vec<Change>, before: Sel, after: Sel, kind: Kind) {
        self.redo.clear();
        let now = Instant::now();
        // keep typing in one step while it's continuous
        if let Some(last) = self.undo.last_mut()
            && kind != Kind::Other
            && last.kind == kind
            && last.after == before
            && now.duration_since(last.at) < Duration::from_millis(1200)
            && last.id != self.saved_state
        {
            last.changes.extend(changes);
            last.after = after;
            last.at = now;
            last.id = self.next_step;
            self.next_step += 1;
        } else {
            let id = self.next_step;
            self.next_step += 1;
            self.undo.push(Step { id, changes, before, after, kind, at: now });
            if self.undo.len() > 2000 {
                self.undo.remove(0);
            }
        }
        self.sel = after;
        self.version += 1;
    }

    /// Replaces the whole text (replace all, big line operations) as one undo step.
    pub fn swap_pieces(&mut self, pieces: Vec<Piece>, new_source: Option<std::sync::Arc<Source>>, sel: Sel) {
        if let Some(s) = new_source {
            self.text.push_source(s);
        }
        let before_sel = self.sel;
        let before = self.text.set_pieces(pieces.clone());
        self.bookmarks.clear();
        self.commit(vec![Change::Swap { before, after: pieces }], before_sel, sel, Kind::Other);
    }

    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else { return false };
        // undo in the reverse of the order they were applied
        for c in step.changes.iter().rev() {
            match c {
                Change::Splice { at, removed, inserted } => {
                    let n: u64 = inserted.iter().map(|p| p.len).sum();
                    self.text.splice(*at, n, removed);
                }
                Change::Swap { before, .. } => {
                    self.text.set_pieces(before.clone());
                }
            }
        }
        self.sel = step.before;
        self.version += 1;
        self.redo.push(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else { return false };
        for c in step.changes.iter() {
            match c {
                Change::Splice { at, removed, inserted } => {
                    let n: u64 = removed.iter().map(|p| p.len).sum();
                    self.text.splice(*at, n, inserted);
                }
                Change::Swap { after, .. } => {
                    self.text.set_pieces(after.clone());
                }
            }
        }
        self.sel = step.after;
        self.version += 1;
        self.undo.push(step);
        true
    }

    // ------------------------------------------------------------ reading helpers

    pub fn len(&self) -> u64 {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn selected_bytes(&self) -> Option<Vec<u8>> {
        let r = self.sel.range();
        if r.end - r.start > OP_MAX { None } else { Some(self.text.read(r)) }
    }

    /// Start of the line holding `x` (bounded look-back on absurdly long lines: falls back to the row).
    pub fn line_start_of(&self, x: u64) -> u64 {
        let look = x.saturating_sub(1 << 20);
        match self.text.rfind_byte(b'\n', look..x) {
            Some(n) => n + 1,
            None if look == 0 => 0,
            None => rows::row_start(&self.text, x),
        }
    }

    /// End of the line holding `x`, before its line break.
    pub fn line_end_of(&self, x: u64) -> u64 {
        let limit = (x + (1 << 20)).min(self.len());
        match self.text.find_byte(b'\n', x..limit) {
            Some(n) => {
                if n > x && self.text.byte(n - 1) == Some(b'\r') { n - 1 } else { n }
            }
            None if limit == self.len() => self.len(),
            None => rows::row_end(&self.text, rows::row_start(&self.text, x)).0,
        }
    }

    /// The whole lines the selection touches: start of the first to just past the last's break.
    pub fn line_span(&self) -> Range<u64> {
        let r = self.sel.range();
        let start = self.line_start_of(r.start);
        // a selection ending at a line start doesn't include that line
        let last = if r.end > r.start && self.line_start_of(r.end) == r.end { r.end - 1 } else { r.end };
        let end_text = self.line_end_of(last.max(start));
        let end = match self.text.byte(end_text) {
            Some(b'\r') if self.text.byte(end_text + 1) == Some(b'\n') => end_text + 2,
            Some(b'\n') | Some(b'\r') => end_text + 1,
            _ => end_text,
        };
        start..end
    }

    /// The leading whitespace of the line holding `x`.
    pub fn indent_of(&self, x: u64) -> Vec<u8> {
        let s = self.line_start_of(x);
        let bytes = self.text.read(s..(s + 256).min(self.len()));
        bytes.iter().take_while(|b| **b == b' ' || **b == b'\t').copied().collect()
    }

    // ------------------------------------------------------------ editing commands

    /// Types `s` over the selection.
    pub fn insert(&mut self, s: &[u8], kind: Kind) {
        let r = self.sel.range();
        let n = s.len() as u64;
        self.edit(vec![(r.clone(), s.to_vec())], kind, |_| Sel::caret(r.start + n));
    }

    /// Typing in overwrite mode: replaces the character under the caret (not a line break).
    pub fn overwrite(&mut self, s: &[u8]) {
        if !self.sel.is_empty() {
            return self.insert(s, Kind::Typing);
        }
        let at = self.sel.head;
        let next = self.next_char(at);
        let end = if matches!(self.text.byte(at), Some(b'\n') | Some(b'\r') | None) { at } else { next };
        let n = s.len() as u64;
        self.edit(vec![(at..end, s.to_vec())], Kind::Typing, |_| Sel::caret(at + n));
    }

    pub fn newline(&mut self, auto_indent: bool) {
        let mut s = self.eol.as_bytes().to_vec();
        if auto_indent {
            s.extend(self.indent_of(self.sel.range().start));
        }
        self.insert(&s, Kind::Other);
    }

    pub fn backspace(&mut self, tab: Option<usize>) {
        if !self.sel.is_empty() {
            return self.delete_selection();
        }
        let at = self.sel.head;
        if at == 0 {
            return;
        }
        // in leading spaces with soft tabs, step back to the previous tab stop
        if let Some(w) = tab {
            let ls = self.line_start_of(at);
            let before = self.text.read(ls..at);
            if !before.is_empty() && before.iter().all(|b| *b == b' ') {
                let n = ((before.len() - 1) % w + 1) as u64;
                return self.edit(vec![(at - n..at, vec![])], Kind::Deleting, |_| Sel::caret(at - n));
            }
        }
        let from = self.prev_char(at);
        self.edit(vec![(from..at, vec![])], Kind::Deleting, |_| Sel::caret(from));
    }

    pub fn delete_forward(&mut self) {
        if !self.sel.is_empty() {
            return self.delete_selection();
        }
        let at = self.sel.head;
        let to = self.next_char(at);
        if to > at {
            self.edit(vec![(at..to, vec![])], Kind::Deleting, |_| Sel::caret(at));
        }
    }

    pub fn delete_selection(&mut self) {
        let r = self.sel.range();
        if !r.is_empty() {
            self.edit(vec![(r.clone(), vec![])], Kind::Other, |_| Sel::caret(r.start));
        }
    }

    /// Deletes to the previous / next word boundary.
    pub fn delete_word(&mut self, forward: bool) {
        if !self.sel.is_empty() {
            return self.delete_selection();
        }
        let at = self.sel.head;
        let to = if forward { self.word_right(at) } else { self.word_left(at) };
        let r = at.min(to)..at.max(to);
        self.edit(vec![(r.clone(), vec![])], Kind::Other, |_| Sel::caret(r.start));
    }

    /// The previous character boundary (UTF-8 aware; "\r\n" counts as one).
    pub fn prev_char(&self, at: u64) -> u64 {
        if at == 0 {
            return 0;
        }
        if at >= 2 && self.text.byte(at - 1) == Some(b'\n') && self.text.byte(at - 2) == Some(b'\r') {
            return at - 2;
        }
        let mut p = at - 1;
        while p > 0 && at - p < 4 && self.text.byte(p).is_some_and(|b| (0x80..0xC0).contains(&b)) {
            p -= 1;
        }
        p
    }

    pub fn next_char(&self, at: u64) -> u64 {
        let len = self.len();
        if at >= len {
            return len;
        }
        if self.text.byte(at) == Some(b'\r') && self.text.byte(at + 1) == Some(b'\n') {
            return at + 2;
        }
        let mut p = at + 1;
        while p < len && p - at < 4 && self.text.byte(p).is_some_and(|b| (0x80..0xC0).contains(&b)) {
            p += 1;
        }
        p
    }

    fn class(b: u8) -> u8 {
        match b {
            b' ' | b'\t' => 0,
            b'\n' | b'\r' => 3,
            b if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 => 1,
            _ => 2,
        }
    }

    /// Word to the left: skip spaces, then a run of the same kind (word characters or punctuation).
    pub fn word_left(&self, at: u64) -> u64 {
        let from = at.saturating_sub(4096);
        let bytes = self.text.read(from..at);
        let mut i = bytes.len();
        if i == 0 {
            return at;
        }
        if Self::class(bytes[i - 1]) == 3 {
            return self.prev_char(at);
        }
        while i > 0 && Self::class(bytes[i - 1]) == 0 {
            i -= 1;
        }
        if i > 0 {
            let c = Self::class(bytes[i - 1]);
            if c != 3 {
                while i > 0 && Self::class(bytes[i - 1]) == c {
                    i -= 1;
                }
            }
        }
        from + i as u64
    }

    pub fn word_right(&self, at: u64) -> u64 {
        let to = (at + 4096).min(self.len());
        let bytes = self.text.read(at..to);
        if bytes.is_empty() {
            return at;
        }
        if Self::class(bytes[0]) == 3 {
            return self.next_char(at);
        }
        let mut i = 0;
        let c = Self::class(bytes[0]);
        if c != 0 {
            while i < bytes.len() && Self::class(bytes[i]) == c {
                i += 1;
            }
        }
        while i < bytes.len() && Self::class(bytes[i]) == 0 {
            i += 1;
        }
        at + i as u64
    }

    /// The word around `at` (for double-click and "find the selected word").
    pub fn word_at(&self, at: u64) -> Range<u64> {
        let from = at.saturating_sub(1024);
        let to = (at + 1024).min(self.len());
        let bytes = self.text.read(from..to);
        let i = (at - from) as usize;
        let c = bytes.get(i).or(bytes.get(i.wrapping_sub(1))).map(|b| Self::class(*b)).unwrap_or(0);
        if c == 3 {
            return at..at;
        }
        let mut s = i;
        while s > 0 && Self::class(bytes[s - 1]) == c {
            s -= 1;
        }
        let mut e = i;
        while e < bytes.len() && Self::class(bytes[e]) == c {
            e += 1;
        }
        from + s as u64..from + e as u64
    }

    /// Indents (or with `out`, unindents) the selected lines.
    pub fn indent(&mut self, out: bool, unit: &[u8], tab: usize) {
        let span = self.line_span();
        if span.end - span.start > OP_MAX {
            return;
        }
        let mut edits = Vec::new();
        let mut s = span.start;
        loop {
            let e = self.line_end_of(s);
            if out {
                let lead = self.text.read(s..(s + tab as u64).min(e));
                let n = if lead.first() == Some(&b'\t') { 1 } else { lead.iter().take_while(|b| **b == b' ').count() };
                if n > 0 {
                    edits.push((s..s + n as u64, vec![]));
                }
            } else if e > s {
                edits.push((s..s, unit.to_vec()));
            }
            let next = match self.text.find_byte(b'\n', e..span.end) {
                Some(n) => n + 1,
                None => break,
            };
            if next >= span.end {
                break;
            }
            s = next;
        }
        let old = self.sel;
        self.edit(edits, Kind::Other, |m| {
            // keep the selection covering the same lines
            if old.anchor <= old.head { Sel { anchor: m(old.anchor), head: m(old.head) } } else { Sel { anchor: m(old.anchor), head: m(old.head) } }
        });
        if !out && self.sel.anchor == self.sel.head {
            // a caret at the line start stays put; move it past the new indent
        }
    }

    /// Runs `f` over the selected lines' text (without the final line break) and puts the result back.
    pub fn map_lines(&mut self, f: impl FnOnce(Vec<&str>) -> Vec<String>) -> bool {
        let span = self.line_span();
        if span.end - span.start > OP_MAX {
            return false;
        }
        let bytes = self.text.read(span.clone());
        let text = String::from_utf8_lossy(&bytes);
        let eol = std::str::from_utf8(self.eol.as_bytes()).unwrap_or("\n");
        let trailing = text.ends_with('\n') || text.ends_with('\r');
        let body = text.trim_end_matches(['\n', '\r']);
        let lines: Vec<&str> = body.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let mut out = f(lines).join(eol);
        if trailing {
            out.push_str(eol);
        }
        let start = span.start;
        let n = out.len() as u64;
        let had_selection = !self.sel.is_empty();
        self.edit(vec![(span, out.into_bytes())], Kind::Other, |_| if had_selection { Sel { anchor: start, head: start + n } } else { Sel::caret(start) });
        true
    }

    pub fn duplicate(&mut self) {
        if !self.sel.is_empty() {
            let r = self.sel.range();
            if let Some(b) = self.selected_bytes() {
                let n = b.len() as u64;
                self.edit(vec![(r.end..r.end, b)], Kind::Other, |_| Sel { anchor: r.end, head: r.end + n });
            }
            return;
        }
        let span = self.line_span();
        if span.end - span.start > OP_MAX {
            return;
        }
        let mut b = self.text.read(span.clone());
        let caret = self.sel.head;
        if !b.ends_with(b"\n") && !b.ends_with(b"\r") {
            let mut e = self.eol.as_bytes().to_vec();
            e.extend(b);
            b = e;
            let n = b.len() as u64;
            self.edit(vec![(span.end..span.end, b)], Kind::Other, |_| Sel::caret(caret + n));
        } else {
            let n = b.len() as u64;
            self.edit(vec![(span.end..span.end, b)], Kind::Other, |_| Sel::caret(caret + n));
        }
    }

    pub fn delete_lines(&mut self) {
        let span = self.line_span();
        let start = span.start;
        self.edit(vec![(span, vec![])], Kind::Other, |_| Sel::caret(start));
    }

    /// Moves the selected lines up or down by one.
    pub fn move_lines(&mut self, down: bool) {
        let span = self.line_span();
        if span.end - span.start > OP_MAX {
            return;
        }
        let eol = self.eol.as_bytes().to_vec();
        let old = self.sel;
        if down {
            if span.end >= self.len() {
                return;
            }
            let next = self.line_start_of(span.end)..{
                let e = self.line_end_of(span.end);
                match self.text.byte(e) {
                    Some(b'\r') if self.text.byte(e + 1) == Some(b'\n') => e + 2,
                    Some(b'\n') => e + 1,
                    _ => e,
                }
            };
            let mut moved = self.text.read(next.clone());
            let mut cur = self.text.read(span.clone());
            if !moved.ends_with(b"\n") {
                moved.extend_from_slice(&eol);
                if cur.ends_with(&eol) {
                    cur.truncate(cur.len() - eol.len());
                }
            }
            let shift = moved.len() as u64;
            moved.extend(cur);
            self.edit(vec![(span.start..next.end, moved)], Kind::Other, |_| Sel { anchor: old.anchor + shift, head: old.head + shift });
        } else {
            if span.start == 0 {
                return;
            }
            let prev = self.line_start_of(span.start - 1)..span.start;
            let mut cur = self.text.read(span.clone());
            let mut moved = self.text.read(prev.clone());
            if !cur.ends_with(b"\n") {
                cur.extend_from_slice(&eol);
                if moved.ends_with(&eol) {
                    moved.truncate(moved.len() - eol.len());
                }
            }
            let shift = prev.end - prev.start;
            cur.extend(moved);
            self.edit(vec![(prev.start..span.end, cur)], Kind::Other, |_| Sel { anchor: old.anchor - shift, head: old.head - shift });
        }
    }

    /// Toggles a line comment on the selected lines.
    pub fn toggle_comment(&mut self, prefix: &str) {
        if prefix.is_empty() {
            return;
        }
        let p = prefix.to_string();
        self.map_lines(|lines| {
            let all = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with(p.trim_end()));
            let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
            lines
                .iter()
                .map(|l| {
                    if l.trim().is_empty() {
                        l.to_string()
                    } else if all {
                        let lead = l.len() - l.trim_start().len();
                        let rest = &l[lead..];
                        let rest = rest.strip_prefix(&p).or_else(|| rest.strip_prefix(p.trim_end())).unwrap_or(rest);
                        format!("{}{}", &l[..lead], rest)
                    } else {
                        format!("{}{}{}", &l[..indent], p, &l[indent..])
                    }
                })
                .collect()
        });
    }

    /// Changes every line break to `eol` (small enough documents only).
    pub fn convert_eol(&mut self, eol: Eol) -> bool {
        if self.len() > OP_MAX {
            return false;
        }
        let all = self.text.read(0..self.len());
        let mut out = Vec::with_capacity(all.len());
        let mut i = 0;
        while i < all.len() {
            match all[i] {
                b'\r' => {
                    out.extend_from_slice(eol.as_bytes());
                    if all.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                }
                b'\n' => out.extend_from_slice(eol.as_bytes()),
                b => out.push(b),
            }
            i += 1;
        }
        self.eol = eol;
        if out != all {
            let n = out.len() as u64;
            self.edit(vec![(0..self.len(), out)], Kind::Other, |_| Sel::caret(0));
            let _ = n;
        }
        true
    }

    /// Toggles a bookmark on the row holding the caret.
    pub fn toggle_bookmark(&mut self) {
        let r = self.line_start_of(self.sel.head);
        match self.bookmarks.binary_search(&r) {
            Ok(i) => {
                self.bookmarks.remove(i);
            }
            Err(i) => self.bookmarks.insert(i, r),
        }
    }

    pub fn next_bookmark(&self, forward: bool) -> Option<u64> {
        let here = self.line_start_of(self.sel.head);
        if forward {
            self.bookmarks.iter().find(|&&b| b > here).or(self.bookmarks.first()).copied()
        } else {
            self.bookmarks.iter().rev().find(|&&b| b < here).or(self.bookmarks.last()).copied()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> Document {
        Document::new(Text::from_bytes(s.as_bytes().to_vec()), None, Encoding::Utf8, Eol::Lf)
    }
    fn text(d: &Document) -> String {
        String::from_utf8(d.text.read(0..d.len())).unwrap()
    }

    #[test]
    fn typing_undo_redo() {
        let mut d = doc("hello");
        d.sel = Sel::caret(5);
        for c in " world".bytes() {
            d.insert(&[c], Kind::Typing);
        }
        assert_eq!(text(&d), "hello world");
        assert!(d.dirty());
        d.undo();
        assert_eq!(text(&d), "hello"); // typing grouped into one step
        assert!(!d.dirty(), "undoing everything is clean again");
        d.redo();
        assert_eq!(text(&d), "hello world");
        d.sel = Sel { anchor: 0, head: 5 };
        d.insert(b"bye", Kind::Other);
        assert_eq!(text(&d), "bye world");
        d.undo();
        assert_eq!(text(&d), "hello world");
        assert_eq!(d.sel, Sel { anchor: 0, head: 5 });
    }

    #[test]
    fn multi_edit_undo() {
        let mut d = doc("a b c");
        d.edit(vec![(0..1, b"xx".to_vec()), (4..5, b"yyy".to_vec())], Kind::Other, |m| Sel::caret(m(5)));
        assert_eq!(text(&d), "xx b yyy");
        assert_eq!(d.sel.head, 8);
        d.undo();
        assert_eq!(text(&d), "a b c");
        d.redo();
        assert_eq!(text(&d), "xx b yyy");
    }

    #[test]
    fn line_ops() {
        let mut d = doc("one\ntwo\nthree\n");
        d.sel = Sel::caret(5);
        d.duplicate();
        assert_eq!(text(&d), "one\ntwo\ntwo\nthree\n");
        d.move_lines(false);
        assert_eq!(text(&d), "one\ntwo\ntwo\nthree\n");
        d.sel = Sel::caret(1);
        d.move_lines(true);
        assert_eq!(text(&d), "two\none\ntwo\nthree\n");
        d.sel = Sel { anchor: 0, head: 7 };
        d.toggle_comment("// ");
        assert_eq!(text(&d), "// two\n// one\ntwo\nthree\n");
        d.toggle_comment("// ");
        assert_eq!(text(&d), "two\none\ntwo\nthree\n");
        d.sel = Sel { anchor: 0, head: 8 };
        d.indent(false, b"    ", 4);
        assert_eq!(text(&d), "    two\n    one\ntwo\nthree\n");
        d.indent(true, b"    ", 4);
        assert_eq!(text(&d), "two\none\ntwo\nthree\n");
        d.sel = Sel::caret(9);
        d.delete_lines();
        assert_eq!(text(&d), "two\none\nthree\n");
        // last line without a newline
        let mut d = doc("a\nb");
        d.sel = Sel::caret(2);
        d.move_lines(false);
        assert_eq!(text(&d), "b\na");
        assert!(d.convert_eol(Eol::CrLf));
        assert_eq!(text(&d), "b\r\na");
    }

    #[test]
    fn words_and_chars() {
        let d = doc("héllo, wörld\r\nx");
        assert_eq!(d.next_char(1), 3); // é is two bytes
        assert_eq!(d.prev_char(3), 1);
        assert_eq!(d.next_char(14), 16); // \r\n is one step
        assert_eq!(d.word_right(0), 6);
        assert_eq!(d.word_left(13), 8);
        assert_eq!(d.word_at(2), 0..6);
    }

    #[test]
    fn backspace_soft_tabs() {
        let mut d = doc("        x");
        d.sel = Sel::caret(8);
        d.backspace(Some(4));
        assert_eq!(text(&d), "    x");
        d.backspace(Some(4));
        assert_eq!(text(&d), "x");
    }
}
