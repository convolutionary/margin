//! Find and replace over any size of document.
//!
//! The text is searched in windows of a few megabytes that end on a line break, so a pattern never
//! has to see the whole file and a match never straddles two windows (matches are within a line,
//! as in most editors). Long searches report progress and can be cancelled.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use regex::bytes::{Regex, RegexBuilder};

use crate::text::{Piece, Text};

const WINDOW: u64 = 4 << 20;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub pattern: String,
    pub regex: bool,
    pub case: bool,
    pub word: bool,
}

impl Query {
    pub fn compile(&self) -> Result<Regex, String> {
        if self.pattern.is_empty() {
            return Err("nothing to find".into());
        }
        let mut p = if self.regex { self.pattern.clone() } else { regex::escape(&self.pattern) };
        if self.word {
            p = format!(r"\b(?:{p})\b");
        }
        RegexBuilder::new(&p)
            .case_insensitive(!self.case)
            .multi_line(true)
            .crlf(true)
            .size_limit(64 << 20)
            .build()
            .map_err(|e| match e {
                regex::Error::Syntax(s) => s.lines().last().unwrap_or("invalid pattern").trim_start_matches("error: ").to_string(),
                other => other.to_string(),
            })
    }
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 { 0.0 } else { (self.done.load(Ordering::Relaxed) as f32 / t as f32).min(1.0) }
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// The window starting at `from`: its bytes and where it ends (on a line break when possible).
fn window(t: &Text, from: u64) -> (Vec<u8>, u64) {
    let mut end = (from + WINDOW).min(t.len());
    if end < t.len()
        && let Some(n) = t.rfind_byte(b'\n', from..end)
    {
        end = n + 1;
    }
    (t.read(from..end), end)
}

/// Start of the line holding `x`, looking back a bounded distance.
fn line_floor(t: &Text, x: u64) -> u64 {
    let look = x.saturating_sub(64 << 10);
    match t.rfind_byte(b'\n', look..x) {
        Some(n) => n + 1,
        None => look,
    }
}

/// Next match at or after `from`, searching up to `to`.
pub fn find_forward(t: &Text, re: &Regex, from: u64, to: u64, p: &Progress) -> Option<Range<u64>> {
    let mut start = line_floor(t, from);
    p.total.store(to.saturating_sub(from), Ordering::Relaxed);
    while start < to {
        if p.cancelled() {
            return None;
        }
        let (buf, end) = window(t, start);
        let skip = from.saturating_sub(start) as usize;
        if skip <= buf.len() {
            let mut at = skip;
            while let Some(m) = re.find_at(&buf, at) {
                if start + m.start() as u64 >= to {
                    return None;
                }
                if m.is_empty() && start + (m.start() as u64) < from {
                    at = m.end() + 1;
                    continue;
                }
                return Some(start + m.start() as u64..start + m.end() as u64);
            }
        }
        if end <= start {
            break;
        }
        start = end;
        p.done.store(start.saturating_sub(from), Ordering::Relaxed);
    }
    None
}

/// Last match that starts before `before`, searching back to `to`.
pub fn find_backward(t: &Text, re: &Regex, before: u64, to: u64, p: &Progress) -> Option<Range<u64>> {
    let mut end = before.min(t.len());
    p.total.store(end.saturating_sub(to), Ordering::Relaxed);
    let total_end = end;
    while end > to {
        if p.cancelled() {
            return None;
        }
        let mut start = end.saturating_sub(WINDOW).max(to);
        if start > to {
            start = line_floor(t, start);
        }
        // read to the end of the line so `$` and word boundaries see the real context
        let line_end = t.find_byte(b'\n', end..(end + (64 << 10)).min(t.len())).unwrap_or(t.len().min(end + (64 << 10)));
        let buf = t.read(start..line_end);
        let limit = (end - start) as usize;
        let mut last = None;
        for m in re.find_iter(&buf) {
            if m.start() >= limit {
                break;
            }
            last = Some(m.range());
        }
        if let Some(r) = last {
            return Some(start + r.start as u64..start + r.end as u64);
        }
        if start <= to {
            break;
        }
        end = start;
        p.done.store(total_end - end, Ordering::Relaxed);
    }
    None
}

/// Next match after `from`, wrapping around to the top.
pub fn find_next(t: &Text, re: &Regex, from: u64, wrap: bool, p: &Progress) -> Option<Range<u64>> {
    find_forward(t, re, from, t.len(), p).or_else(|| if wrap && from > 0 && !p.cancelled() { find_forward(t, re, 0, from, p) } else { None })
}

pub fn find_prev(t: &Text, re: &Regex, before: u64, wrap: bool, p: &Progress) -> Option<Range<u64>> {
    find_backward(t, re, before, 0, p).or_else(|| if wrap && !p.cancelled() { find_backward(t, re, t.len(), before, p) } else { None })
}

/// Every match (up to `limit`), with a callback per window so callers can show results as they come.
pub fn find_all(t: &Text, re: &Regex, limit: usize, p: &Progress, mut found: impl FnMut(&[Range<u64>])) -> usize {
    let mut start = 0;
    let mut n = 0;
    p.total.store(t.len(), Ordering::Relaxed);
    let mut batch = Vec::new();
    while start < t.len() && n < limit {
        if p.cancelled() {
            break;
        }
        let (buf, end) = window(t, start);
        batch.clear();
        for m in re.find_iter(&buf) {
            batch.push(start + m.start() as u64..start + m.end() as u64);
            n += 1;
            if n >= limit {
                break;
            }
        }
        if !batch.is_empty() {
            found(&batch);
        }
        if end <= start {
            break;
        }
        start = end;
        p.done.store(start, Ordering::Relaxed);
    }
    n
}

/// The replacement for one match: `$1`-style groups expand when searching with a regex.
pub fn expand(re: &Regex, regex: bool, hay: &[u8], replacement: &str) -> Vec<u8> {
    if !regex {
        return replacement.as_bytes().to_vec();
    }
    let mut out = Vec::new();
    if let Some(c) = re.captures(hay) {
        c.expand(unescape(replacement).as_bytes(), &mut out);
    }
    out
}

/// `\n`, `\t` and `\\` in a regex replacement.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// What replace-all produces: a new piece list (old content between matches, replacements from one
/// new source) and how many matches it replaced. One pass, whatever the size of the document.
pub struct Replaced {
    pub pieces: Vec<Piece>,
    pub added: Vec<u8>,
    pub count: usize,
}

/// Replaces every match in `range`. `src` is the source index the caller will give `added`.
pub fn replace_all(t: &Text, re: &Regex, regex: bool, replacement: &str, range: Range<u64>, src: u32, p: &Progress) -> Option<Replaced> {
    let mut pieces = Vec::new();
    let mut added: Vec<u8> = Vec::new();
    let mut count = 0;
    let mut copied = 0u64; // document offset copied up to
    t.pieces_in(0..range.start, &mut pieces);
    copied = copied.max(range.start);
    let mut start = range.start;
    p.total.store(range.end - range.start, Ordering::Relaxed);
    while start < range.end {
        if p.cancelled() {
            return None;
        }
        let (buf, end) = window(t, start);
        let end = end.min(range.end);
        let buf = &buf[..(end - start) as usize];
        for c in re.find_iter(buf) {
            let (ms, me) = (start + c.start() as u64, start + c.end() as u64);
            if c.is_empty() && ms < copied {
                continue;
            }
            t.pieces_in(copied..ms, &mut pieces);
            let rep = if regex { expand(re, true, &buf[c.start()..c.end()], replacement) } else { replacement.as_bytes().to_vec() };
            if !rep.is_empty() {
                let piece = Piece { src, start: added.len() as u64, len: rep.len() as u64 };
                added.extend_from_slice(&rep);
                match pieces.last_mut() {
                    Some(l) if l.src == src && l.start + l.len == piece.start => l.len += piece.len,
                    _ => pieces.push(piece),
                }
            }
            copied = me;
            count += 1;
        }
        if end <= start {
            break;
        }
        start = end;
        p.done.store(start - range.start, Ordering::Relaxed);
    }
    t.pieces_in(copied..t.len(), &mut pieces);
    Some(Replaced { pieces, added, count })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Source;

    fn q(p: &str) -> Regex {
        Query { pattern: p.into(), ..Default::default() }.compile().unwrap()
    }

    #[test]
    fn find_both_ways() {
        let t = Text::from_bytes(b"one Two three\nTWO two\n".to_vec());
        let re = q("two");
        let p = Progress::default();
        assert_eq!(find_next(&t, &re, 0, true, &p), Some(4..7));
        assert_eq!(find_next(&t, &re, 5, true, &p), Some(14..17));
        assert_eq!(find_next(&t, &re, 19, true, &p), Some(4..7)); // wraps
        assert_eq!(find_prev(&t, &re, 14, true, &p), Some(4..7));
        assert_eq!(find_prev(&t, &re, 4, true, &p), Some(18..21)); // wraps
        let cased = Query { pattern: "two".into(), case: true, ..Default::default() }.compile().unwrap();
        assert_eq!(find_next(&t, &cased, 0, false, &p), Some(18..21));
        let word = Query { pattern: "on".into(), word: true, ..Default::default() }.compile().unwrap();
        assert_eq!(find_next(&t, &word, 0, false, &p), None);
        let mut all = vec![];
        find_all(&t, &re, 100, &p, |b| all.extend_from_slice(b));
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn replace_everything() {
        let mut t = Text::from_bytes(b"a1 b22 c333\nd4\n".to_vec());
        let re = Query { pattern: r"(\d+)".into(), regex: true, ..Default::default() }.compile().unwrap();
        let src = t.sources().len() as u32;
        let r = replace_all(&t, &re, true, "<$1>", 0..t.len(), src, &Progress::default()).unwrap();
        assert_eq!(r.count, 4);
        let n = r.added.len() as u64;
        t.push_source(Source::owned(r.added));
        let _ = n;
        t.set_pieces(r.pieces);
        assert_eq!(t.read(0..t.len()), b"a<1> b<22> c<333>\nd<4>\n");
    }

    #[test]
    fn big_windows() {
        // more than one window, matches near the edges
        let mut v = Vec::new();
        for i in 0..400_000 {
            v.extend_from_slice(format!("line {i} needle{}\n", i % 1000).as_bytes());
        }
        let t = Text::from_bytes(v);
        let re = q("needle999\n");
        let p = Progress::default();
        let mut n = 0;
        find_all(&t, &re, usize::MAX, &p, |b| n += b.len());
        assert_eq!(n, 400);
        let last = find_prev(&t, &re, t.len(), false, &p).unwrap();
        assert_eq!(&t.read(last.clone()), b"needle999\n");
        let first = find_next(&t, &re, 0, false, &p).unwrap();
        assert!(first.start < last.start);
    }
}
