//! Rows: what the editor draws one of per line.
//!
//! A row is a line, except that a very long line (a minified file, a 40 GB log with no newlines) is
//! cut into rows of about `SEG` bytes. The cuts sit at fixed document offsets, so the row holding any
//! byte can be found by looking back at most two rows, never by scanning back to a line start that
//! may be gigabytes away.
//!
//! Rule: rows start after every newline, and at every multiple of SEG (moved forward onto a UTF-8
//! character boundary) that has no newline in the SEG bytes before it.

use crate::text::Text;

pub const SEG: u64 = 4096;

/// `p` moved forward past UTF-8 continuation bytes (at most 3), so a cut never splits a character.
fn on_char(t: &Text, p: u64) -> u64 {
    let mut q = p;
    while q < p + 3 && t.byte(q).is_some_and(|b| (0x80..0xC0).contains(&b)) {
        q += 1;
    }
    q
}

/// Start of the row containing byte `x` (x == len gives the last row).
pub fn row_start(t: &Text, x: u64) -> u64 {
    let x = x.min(t.len());
    if x == 0 {
        return 0;
    }
    let look = x.saturating_sub(2 * SEG + 4);
    let line = match t.rfind_byte(b'\n', look..x) {
        Some(n) => Some(n + 1),
        None if look == 0 => Some(0),
        None => None,
    };
    let aligned = |x: u64| {
        let mut p = x / SEG * SEG;
        if p > 0 && on_char(t, p) > x {
            p -= SEG;
        }
        p
    };
    match line {
        Some(l) => {
            let first = (l + SEG).div_ceil(SEG) * SEG; // first cut inside this line
            if x < on_char(t, first) {
                l
            } else {
                on_char(t, aligned(x).max(first))
            }
        }
        None => on_char(t, aligned(x)),
    }
}

/// Where the row starting at `s` ends: (end of its text, start of the next row). The text end leaves
/// out the line break ("\n" or "\r\n"). The next start is None for the last row.
pub fn row_end(t: &Text, s: u64) -> (u64, Option<u64>) {
    let len = t.len();
    let is_line_start = s == 0 || t.byte(s - 1) == Some(b'\n');
    let cut = if is_line_start { (s + SEG).div_ceil(SEG) * SEG } else { (s / SEG + 1) * SEG };
    let cut = on_char(t, cut.max(s + 1));
    match t.find_byte(b'\n', s..cut.min(len)) {
        Some(n) => {
            let text_end = if n > s && t.byte(n - 1) == Some(b'\r') { n - 1 } else { n };
            (text_end, Some(n + 1))
        }
        None if cut >= len => (len, None),
        None => (cut, Some(cut)),
    }
}

pub fn next_row(t: &Text, s: u64) -> Option<u64> {
    row_end(t, s).1
}

pub fn prev_row(t: &Text, s: u64) -> Option<u64> {
    if s == 0 { None } else { Some(row_start(t, s - 1)) }
}

/// Whether the row at `s` begins a line (as opposed to continuing a long one).
pub fn starts_line(t: &Text, s: u64) -> bool {
    s == 0 || t.byte(s - 1) == Some(b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(t: &Text) -> Vec<u64> {
        let mut out = vec![0];
        let mut s = 0;
        while let Some(n) = next_row(t, s) {
            out.push(n);
            s = n;
        }
        out
    }

    #[test]
    fn short_lines() {
        let t = Text::from_bytes(b"ab\r\ncd\n\nlast".to_vec());
        assert_eq!(rows(&t), vec![0, 4, 7, 8]);
        assert_eq!(row_end(&t, 0), (2, Some(4)));
        assert_eq!(row_end(&t, 8), (12, None));
        for x in 0..=t.len() {
            let r = row_start(&t, x);
            assert!(rows(&t).contains(&r));
        }
        // a trailing newline gives a last, empty row
        let t = Text::from_bytes(b"a\n".to_vec());
        assert_eq!(rows(&t), vec![0, 2]);
        assert_eq!(row_end(&t, 2), (2, None));
    }

    #[test]
    fn long_lines_agree_both_ways() {
        // a long line, a short one, a very long one with multibyte characters
        let mut v = vec![b'a'; 10_000];
        v.push(b'\n');
        v.extend_from_slice(b"short\n");
        for _ in 0..9000 {
            v.extend_from_slice("é".as_bytes());
        }
        let t = Text::from_bytes(v);
        let all = rows(&t);
        for w in all.windows(2) {
            assert!(w[1] - w[0] <= 2 * SEG + 4, "row too long: {w:?}");
            assert!(std::str::from_utf8(&t.read(w[0]..w[1])).is_ok(), "cut inside a character at {}", w[1]);
        }
        // every byte maps back to the row that forward iteration produced
        for x in (0..=t.len()).step_by(7) {
            let r = row_start(&t, x);
            let i = all.partition_point(|&s| s <= x) - 1;
            assert_eq!(r, all[i], "byte {x}");
            if r > 0 {
                assert_eq!(next_row(&t, prev_row(&t, r).unwrap()), Some(r));
            }
        }
    }
}
