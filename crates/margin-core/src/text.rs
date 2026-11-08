//! The document's bytes: a piece table over immutable sources.
//!
//! The file on disk is one source (memory-mapped when it's big, so opening a 300 GB file costs
//! nothing). Everything typed or pasted lands in append-only "add" sources. The document is a list of
//! pieces, each a range of some source. Editing only rewrites that list, so an edit in the middle of
//! a huge file is as cheap as one in a small file, and undo just puts old pieces back.

use std::ops::{ControlFlow, Range};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::index::LineIndex;

/// Bytes of a source: mapped from a file, or owned.
pub enum Bytes {
    Map(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

/// A source plus, for big frozen ones, an index of where its lines start.
pub struct Source {
    pub bytes: Bytes,
    pub index: Option<Arc<LineIndex>>,
}

impl Source {
    pub fn owned(v: Vec<u8>) -> Arc<Source> {
        Arc::new(Source { bytes: Bytes::Owned(v), index: None })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub src: u32,
    pub start: u64,
    pub len: u64,
}

/// Add sources are capped so a snapshot never holds on to a huge growing buffer.
const ADD_CHUNK: usize = 4 << 20;
/// Sources at least this big get a line index; smaller ones are counted on the spot.
pub const INDEX_MIN: u64 = 1 << 20;

/// A cheap, cloneable view of the document's content. Clones share everything; a clone handed to a
/// search or save thread stays valid while the document keeps changing.
#[derive(Clone)]
pub struct Text {
    sources: Vec<Arc<Source>>,
    pieces: Arc<Vec<Piece>>,
    /// document offset where each piece starts
    starts: Arc<Vec<u64>>,
    len: u64,
    /// newlines before each piece, filled in lazily, for line numbers
    nl: Arc<Mutex<Vec<u64>>>,
    /// the source typing appends to (an index into `sources`)
    add: Option<u32>,
}

impl Default for Text {
    fn default() -> Self {
        Text::from_source(Source::owned(Vec::new()), 0..0)
    }
}

impl Text {
    /// A document showing `range` of `src`.
    pub fn from_source(src: Arc<Source>, range: Range<u64>) -> Text {
        let pieces = if range.is_empty() { vec![] } else { vec![Piece { src: 0, start: range.start, len: range.end - range.start }] };
        let mut t = Text {
            sources: vec![src],
            pieces: Arc::new(pieces),
            starts: Arc::new(vec![]),
            len: 0,
            nl: Arc::new(Mutex::new(Vec::new())),
            add: None,
        };
        t.restart(0);
        t
    }

    pub fn from_bytes(v: Vec<u8>) -> Text {
        let n = v.len() as u64;
        Text::from_source(Source::owned(v), 0..n)
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub fn source(&self, i: u32) -> &Arc<Source> {
        &self.sources[i as usize]
    }

    pub fn sources(&self) -> &[Arc<Source>] {
        &self.sources
    }

    fn piece_bytes(&self, p: &Piece) -> &[u8] {
        let b = &self.sources[p.src as usize].bytes;
        &b[p.start as usize..(p.start + p.len) as usize]
    }

    /// Recomputes piece starts from piece `from` on, and forgets cached line counts past it.
    fn restart(&mut self, from: usize) {
        let starts = Arc::make_mut(&mut self.starts);
        starts.truncate(from);
        let mut at = if from == 0 { 0 } else { starts[from - 1] + self.pieces[from - 1].len };
        for p in &self.pieces[from..] {
            starts.push(at);
            at += p.len;
        }
        self.len = at;
        // a fresh cache: clones made before this edit keep theirs
        let mut nl = self.nl.lock().clone();
        nl.truncate(from);
        self.nl = Arc::new(Mutex::new(nl));
    }

    /// The piece holding `off` and the offset within it. `off == len` gives (pieces.len(), 0).
    fn locate(&self, off: u64) -> (usize, u64) {
        if off >= self.len {
            return (self.pieces.len(), 0);
        }
        let i = self.starts.partition_point(|&s| s <= off) - 1;
        (i, off - self.starts[i])
    }

    /// Calls `f` with consecutive slices covering `range`, until it says stop.
    pub fn chunks<B>(&self, range: Range<u64>, mut f: impl FnMut(u64, &[u8]) -> ControlFlow<B>) -> Option<B> {
        let end = range.end.min(self.len);
        if range.start >= end {
            return None;
        }
        let (mut i, mut within) = self.locate(range.start);
        let mut at = range.start;
        while at < end && i < self.pieces.len() {
            let p = &self.pieces[i];
            let bytes = self.piece_bytes(p);
            let take = ((p.len - within).min(end - at)) as usize;
            if let ControlFlow::Break(b) = f(at, &bytes[within as usize..within as usize + take]) {
                return Some(b);
            }
            at += take as u64;
            i += 1;
            within = 0;
        }
        None
    }

    /// Like `chunks`, from the end of `range` backwards.
    pub fn chunks_rev<B>(&self, range: Range<u64>, mut f: impl FnMut(u64, &[u8]) -> ControlFlow<B>) -> Option<B> {
        let end = range.end.min(self.len);
        if range.start >= end {
            return None;
        }
        let (mut i, mut within) = self.locate(end - 1);
        within += 1; // exclusive end inside piece i
        let mut at = end;
        loop {
            let p = &self.pieces[i];
            let bytes = self.piece_bytes(p);
            let take = within.min(at - range.start);
            let from = (within - take) as usize;
            at -= take;
            if let ControlFlow::Break(b) = f(at, &bytes[from..within as usize]) {
                return Some(b);
            }
            if at <= range.start || i == 0 {
                return None;
            }
            i -= 1;
            within = self.pieces[i].len;
        }
    }

    /// Copies `range` out. Callers keep ranges small (a screen, a selection they checked).
    pub fn read(&self, range: Range<u64>) -> Vec<u8> {
        let mut out = Vec::with_capacity((range.end.min(self.len).saturating_sub(range.start)) as usize);
        self.chunks::<()>(range, |_, c| {
            out.extend_from_slice(c);
            ControlFlow::Continue(())
        });
        out
    }

    pub fn byte(&self, off: u64) -> Option<u8> {
        if off >= self.len {
            return None;
        }
        let (i, w) = self.locate(off);
        Some(self.piece_bytes(&self.pieces[i])[w as usize])
    }

    /// First position of `b` in `range`.
    pub fn find_byte(&self, b: u8, range: Range<u64>) -> Option<u64> {
        self.chunks(range, |at, c| match memchr::memchr(b, c) {
            Some(i) => ControlFlow::Break(at + i as u64),
            None => ControlFlow::Continue(()),
        })
    }

    /// Last position of `b` in `range`.
    pub fn rfind_byte(&self, b: u8, range: Range<u64>) -> Option<u64> {
        self.chunks_rev(range, |at, c| match memchr::memrchr(b, c) {
            Some(i) => ControlFlow::Break(at + i as u64),
            None => ControlFlow::Continue(()),
        })
    }

    // ------------------------------------------------------------ editing

    /// Makes `off` a piece boundary; returns the index of the piece starting there.
    fn split(&mut self, off: u64) -> usize {
        let (i, within) = self.locate(off);
        if within == 0 {
            return i;
        }
        let pieces = Arc::make_mut(&mut self.pieces);
        let p = pieces[i];
        pieces[i].len = within;
        pieces.insert(i + 1, Piece { src: p.src, start: p.start + within, len: p.len - within });
        let at = self.starts[i] + within;
        Arc::make_mut(&mut self.starts).insert(i + 1, at);
        i + 1
    }

    /// Replaces `remove` bytes at `at` with `insert` pieces; returns the pieces taken out.
    /// The primitive behind every edit, undo and redo.
    pub fn splice(&mut self, at: u64, remove: u64, insert: &[Piece]) -> Vec<Piece> {
        let at = at.min(self.len);
        let remove = remove.min(self.len - at);
        let i = self.split(at);
        let j = if remove == 0 { i } else { self.split(at + remove) };
        let pieces = Arc::make_mut(&mut self.pieces);
        let before = pieces.len();
        let removed: Vec<Piece> = pieces.splice(i..j, insert.iter().copied().filter(|p| p.len > 0)).collect();
        let added = pieces.len() + (j - i) - before;
        // join neighbours that continue each other (typing a word makes one piece, not one per key)
        let lo = i.saturating_sub(1);
        let mut hi = (i + added).min(pieces.len().saturating_sub(1));
        let mut k = lo;
        while k < hi {
            let (a, b) = (pieces[k], pieces[k + 1]);
            if a.src == b.src && a.start + a.len == b.start {
                pieces[k].len += b.len;
                pieces.remove(k + 1);
                hi -= 1;
            } else {
                k += 1;
            }
        }
        let lo = lo.min(pieces.len());
        self.restart(lo);
        removed
    }

    /// Stores `bytes` in an add source and returns pieces pointing at them.
    pub fn store(&mut self, bytes: &[u8]) -> Vec<Piece> {
        if bytes.is_empty() {
            return vec![];
        }
        if bytes.len() > ADD_CHUNK / 2 {
            // a big paste is a source of its own
            let i = self.push_source(big_source(bytes.to_vec()));
            return vec![Piece { src: i, start: 0, len: bytes.len() as u64 }];
        }
        if let Some(a) = self.add {
            let src = &mut self.sources[a as usize];
            if let Some(s) = Arc::get_mut(src)
                && let Bytes::Owned(v) = &mut s.bytes
                && v.len() + bytes.len() <= ADD_CHUNK
            {
                let start = v.len() as u64;
                v.extend_from_slice(bytes);
                return vec![Piece { src: a, start, len: bytes.len() as u64 }];
            }
        }
        // no add source yet, it's full, or a snapshot is still reading it: start another
        let mut v = Vec::with_capacity(ADD_CHUNK.min(bytes.len().max(64 << 10)));
        v.extend_from_slice(bytes);
        let i = self.push_source(Source::owned(v));
        self.add = Some(i);
        vec![Piece { src: i, start: 0, len: bytes.len() as u64 }]
    }

    pub fn push_source(&mut self, s: Arc<Source>) -> u32 {
        self.sources.push(s);
        (self.sources.len() - 1) as u32
    }

    /// Replaces the whole piece list (replace-all, reload). Returns the old one.
    pub fn set_pieces(&mut self, pieces: Vec<Piece>) -> Vec<Piece> {
        let old = std::mem::replace(Arc::make_mut(&mut self.pieces), pieces);
        self.restart(0);
        old
    }

    /// The pieces covering `range`, cut to fit.
    pub fn pieces_in(&self, range: Range<u64>, out: &mut Vec<Piece>) {
        let end = range.end.min(self.len);
        if range.start >= end {
            return;
        }
        let (mut i, mut within) = self.locate(range.start);
        let mut at = range.start;
        while at < end {
            let p = self.pieces[i];
            let take = (p.len - within).min(end - at);
            let piece = Piece { src: p.src, start: p.start + within, len: take };
            match out.last_mut() {
                Some(l) if l.src == piece.src && l.start + l.len == piece.start => l.len += take,
                _ => out.push(piece),
            }
            at += take;
            i += 1;
            within = 0;
        }
    }

    // ------------------------------------------------------------ lines

    /// Newlines in one piece, or None while its source is still being indexed.
    fn piece_newlines(&self, p: &Piece) -> Option<u64> {
        let s = &self.sources[p.src as usize];
        match &s.index {
            Some(ix) => ix.count(&s.bytes, p.start..p.start + p.len),
            None => Some(bytecount::count(self.piece_bytes(p), b'\n') as u64),
        }
    }

    /// Newlines before each piece up to `upto` (exclusive), extending the cache. None if a piece on
    /// the way isn't indexed yet.
    fn nl_upto(&self, upto: usize) -> Option<u64> {
        let mut nl = self.nl.lock();
        if nl.is_empty() && !self.pieces.is_empty() {
            nl.push(0);
        }
        while nl.len() <= upto && nl.len() <= self.pieces.len() {
            let k = nl.len();
            let before = nl[k - 1] + self.piece_newlines(&self.pieces[k - 1])?;
            nl.push(before);
        }
        if self.pieces.is_empty() {
            return Some(0);
        }
        nl.get(upto).copied()
    }

    /// Number of newlines before `off`: the 0-based line number of `off`. None while indexing.
    pub fn line_of(&self, off: u64) -> Option<u64> {
        let off = off.min(self.len);
        let (i, within) = self.locate(off);
        let before = self.nl_upto(i)?;
        if within == 0 {
            return Some(before);
        }
        let p = &self.pieces[i];
        let part = Piece { src: p.src, start: p.start, len: within };
        Some(before + self.piece_newlines(&part)?)
    }

    /// Total lines (a trailing newline starts one more, empty line). None while indexing.
    pub fn line_count(&self) -> Option<u64> {
        self.line_of(self.len).map(|n| n + 1)
    }

    /// Where 0-based line `line` starts. None while indexing or past the end.
    pub fn line_start(&self, line: u64) -> Option<u64> {
        if line == 0 {
            return Some(0);
        }
        // the piece where the line'th newline lives
        let total = self.nl_upto(self.pieces.len())?;
        if line > total {
            return None;
        }
        let nl = self.nl.lock().clone();
        let i = nl.partition_point(|&n| n < line) - 1; // nl[i] < line <= nl[i+1]
        let p = &self.pieces[i];
        let k = line - nl[i]; // the k'th newline (1-based) inside this piece
        let s = &self.sources[p.src as usize];
        let pos = match &s.index {
            Some(ix) => ix.nth_newline(&s.bytes, p.start..p.start + p.len, k)?,
            None => memchr::memchr_iter(b'\n', self.piece_bytes(p)).nth((k - 1) as usize).map(|x| p.start + x as u64)?,
        };
        Some(self.starts[i] + (pos - p.start) + 1)
    }

    /// Whether every source with an index has finished indexing.
    pub fn indexed(&self) -> bool {
        self.pieces.iter().all(|p| self.sources[p.src as usize].index.as_ref().is_none_or(|ix| ix.done()))
    }

    /// Indexing progress across indexed sources, 0..=1.
    pub fn index_progress(&self) -> f32 {
        let (mut done, mut total) = (0u64, 0u64);
        for s in &self.sources {
            if let Some(ix) = &s.index {
                done += ix.scanned();
                total += s.bytes.len() as u64;
            }
        }
        if total == 0 { 1.0 } else { done as f32 / total as f32 }
    }
}

/// An owned source big enough to deserve an index (built right away; it's already in memory).
pub fn big_source(v: Vec<u8>) -> Arc<Source> {
    let index = if v.len() as u64 >= INDEX_MIN { Some(Arc::new(LineIndex::build(&v))) } else { None };
    Arc::new(Source { bytes: Bytes::Owned(v), index })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: &Text) -> String {
        String::from_utf8(t.read(0..t.len())).unwrap()
    }

    #[test]
    fn edits() {
        let mut t = Text::from_bytes(b"hello world".to_vec());
        let ins = t.store(b"big ");
        t.splice(6, 0, &ins);
        assert_eq!(s(&t), "hello big world");
        let removed = t.splice(0, 6, &[]);
        assert_eq!(s(&t), "big world");
        // undo: put back what was removed
        t.splice(0, 0, &removed);
        assert_eq!(s(&t), "hello big world");
        // typing coalesces
        for c in b"!!!" {
            let p = t.store(&[*c]);
            let end = t.len();
            t.splice(end, 0, &p);
        }
        assert_eq!(s(&t), "hello big world!!!");
        assert!(t.pieces().len() <= 4, "{:?}", t.pieces());
        assert_eq!(t.find_byte(b'w', 0..t.len()), Some(10));
        assert_eq!(t.rfind_byte(b'o', 0..t.len()), Some(11));
        assert_eq!(t.byte(4), Some(b'o'));
    }

    #[test]
    fn lines() {
        let mut t = Text::from_bytes(b"a\nbb\nccc\n".to_vec());
        assert_eq!(t.line_count(), Some(4));
        assert_eq!(t.line_of(3), Some(1));
        assert_eq!(t.line_start(2), Some(5));
        assert_eq!(t.line_start(3), Some(9));
        assert_eq!(t.line_start(4), None);
        let p = t.store(b"x\ny\n");
        t.splice(2, 0, &p);
        assert_eq!(s(&t), "a\nx\ny\nbb\nccc\n");
        assert_eq!(t.line_count(), Some(6));
        assert_eq!(t.line_start(3), Some(6));
        assert_eq!(t.line_of(7), Some(3));
    }

    #[test]
    fn reverse_chunks() {
        let mut t = Text::from_bytes(b"0123456789".to_vec());
        let p = t.store(b"ab");
        t.splice(5, 0, &p);
        let mut got = Vec::new();
        t.chunks_rev::<()>(2..9, |at, c| {
            got.push((at, c.to_vec()));
            ControlFlow::Continue(())
        });
        let flat: Vec<u8> = got.iter().rev().flat_map(|(_, c)| c.clone()).collect();
        assert_eq!(flat, b"234ab56");
    }
}
